package com.fauna.app.service

import androidx.lifecycle.LifecycleOwner
import com.fauna.app.core.ApiClient
import com.fauna.app.testing.FaunaRobolectricTestRunner
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.inOrder
import org.mockito.Mockito.mock
import org.mockito.Mockito.timeout
import org.mockito.Mockito.verify
import org.robolectric.annotation.Config

/**
 * The android seat of the shared account-runtime foreground signal
 * (`p2p-shared-set-build.md` § Phone peers — design, decision 2): the process
 * lifecycle sends `false` on `onStop` and `true` on `onStart`, and the seed
 * sends the process's real place before the runtime starts.
 * The calls ride a coroutine, so every assert is a bounded `timeout` verify
 * (testing.md convention 14), never a fixed sleep.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AccountRuntimeForegroundTest {

    private companion object {
        /** A generous ceiling, not an expected latency. */
        const val BUDGET_MS = 10_000L
    }

    @Test
    fun onStopSendsFalse() {
        val api = mock(ApiClient::class.java)
        AccountRuntimeForeground(api).onStop(mock(LifecycleOwner::class.java))
        runBlocking { verify(api, timeout(BUDGET_MS)).setAccountRuntimeForeground(false) }
    }

    @Test
    fun onStartSendsTrue() {
        val api = mock(ApiClient::class.java)
        AccountRuntimeForeground(api).onStart(mock(LifecycleOwner::class.java))
        runBlocking { verify(api, timeout(BUDGET_MS)).setAccountRuntimeForeground(true) }
    }

    @Test
    fun aBackgroundThenForegroundReachesSharedRustInThatOrder() {
        val api = mock(ApiClient::class.java)
        val owner = mock(LifecycleOwner::class.java)
        val observer = AccountRuntimeForeground(api)
        observer.onStop(owner)
        observer.onStart(owner)
        runBlocking {
            val order = inOrder(api)
            order.verify(api, timeout(BUDGET_MS)).setAccountRuntimeForeground(false)
            order.verify(api, timeout(BUDGET_MS)).setAccountRuntimeForeground(true)
        }
    }

    @Test
    fun aProcessWithNoActivityStartedIsNotInFront() {
        // A push or a worker starts the process with no activity: no onStart
        // ever fires, so only the seed keeps the runtime from the Rust default.
        assertFalse(AccountRuntimeForeground.processInFront())
    }

    @Test
    fun theSeedSendsTheProcessPlace() {
        val sent = mutableListOf<Boolean>()
        runBlocking {
            AccountRuntimeForeground.seed(inFront = { false }) { sent.add(it) }
            AccountRuntimeForeground.seed(inFront = { true }) { sent.add(it) }
        }
        assertEquals(listOf(false, true), sent)
    }
}
