package com.fauna.app.ui.viewmodel

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.feed.FeedManagerHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * A blank trained-topic name says why (`docs/goal/behavior/topic-factors.md`
 * § Authoring surface & picker): the create is refused before the round trip
 * and the page's `error-message` carries the shared
 * `personalization.trained_factor_blank_name` sentence — never a silent no-op,
 * as on tui, linux and web. FFI-free: [ApiClient] is mocked, and the strings
 * come from the app's real resources through Robolectric.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class TrainedTopicsVMBlankNameTest {

    private val context = ApplicationProvider.getApplicationContext<android.app.Application>()

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun vmOver(api: ApiClient): TrainedTopicsVM {
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        return TrainedTopicsVM(api, mock(FeedManagerHost::class.java), context)
    }

    @Test
    fun aBlankNamePaintsTheSharedRefusalAndMakesNoCall() {
        val api = mock(ApiClient::class.java)
        val vm = vmOver(api)

        vm.onTopicNameChange("   ")
        vm.submitTopic()

        assertEquals(
            context.getString(R.string.personalization_trained_factor_blank_name),
            vm.errorMessage.value,
        )
        assertFalse("a refused create never goes busy", vm.busy.value)
        runBlocking { verify(api, never()).trainedTopicsCreate(anyString()) }
    }

    @Test
    fun anEmptyNameIsRefusedTheSameWay() {
        val api = mock(ApiClient::class.java)
        val vm = vmOver(api)

        vm.submitTopic()

        assertEquals(
            context.getString(R.string.personalization_trained_factor_blank_name),
            vm.errorMessage.value,
        )
        runBlocking { verify(api, never()).trainedTopicsCreate(anyString()) }
    }
}
