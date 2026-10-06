package com.fauna.app.core

import com.fauna.ffi.FfiContentPolicy
import com.fauna.ffi.FfiFamilyContentNotice
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.ArgumentCaptor
import org.mockito.Mockito.mock
import org.mockito.Mockito.timeout
import org.mockito.Mockito.verify
import org.mockito.Mockito.verifyNoInteractions
import org.mockito.Mockito.`when` as whenever
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_core.ContentLabelEntry

/**
 * The android **Guardian Notify** ward-side counter ([FamilyNotifyStore],
 * family-safety.md § Guardian Notify) — the twin of linux `content_policy.rs`'s
 * `NotifyAccumulator` tests and web's `familyNotify.ts` behavior. [record]
 * calls the real shared `guardianEnforcedCategories` over UniFFI, so this runs
 * via `just android-host-test` (host JNA), like [ContentPolicyInputsTest].
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FamilyNotifyStoreTest {

    private fun label(category: String, permille: Int) =
        ContentLabelEntry(category = category, confidencePerMille = permille.toUShort())

    private fun floors(nsfw: String = "inherit", spam: String = "inherit", phishing: String = "inherit", commercial: String = "inherit") =
        FfiContentPolicy(nsfw = nsfw, spam = spam, phishing = phishing, commercial = commercial)

    // Raw ArgumentCaptor.capture()/any() return null for reference-typed
    // generics, which crashes against Kotlin's non-null parameter types
    // (familyNotifyReport's List, registerCloser's closer) — this codebase has
    // no mockito-kotlin, so the standard unchecked-cast workaround is inline.
    private fun <T> capture(captor: ArgumentCaptor<T>): T {
        captor.capture()
        @Suppress("UNCHECKED_CAST")
        return null as T
    }

    private fun make(inputs: ContentPolicyInputs): Triple<FamilyNotifyStore, ApiClient, AccountStores> {
        val api = mock(ApiClient::class.java)
        val contentPolicyStore = mock(ContentPolicyStore::class.java)
        whenever(contentPolicyStore.inputs).thenReturn(MutableStateFlow(inputs))
        val accountStores = mock(AccountStores::class.java)
        val store = FamilyNotifyStore(api, contentPolicyStore, accountStores)
        return Triple(store, api, accountStores)
    }

    @Test
    fun `record is a no-op when content_notify is off`() {
        val (store, api, _) = make(ContentPolicyInputs(contentPolicy = floors(nsfw = "block"), contentNotify = false))

        store.record("post-1", listOf(label("nsfw", 700)))
        store.flushIfDue()

        verifyNoInteractions(api)
    }

    @Test
    fun `record is a no-op with no guardian content policy`() {
        val (store, api, _) = make(ContentPolicyInputs(contentPolicy = null, contentNotify = true))

        store.record("post-1", listOf(label("nsfw", 700)))
        store.flushIfDue()

        verifyNoInteractions(api)
    }

    @Test
    fun `record is a no-op when no label trips the guardian floor`() {
        // "inherit" on every category never triggers guardianEnforcedCategories
        // (family-safety.md § Guardian Notify: only the GUARDIAN floor counts).
        val (store, api, _) = make(ContentPolicyInputs(contentPolicy = floors(), contentNotify = true))

        store.record("post-1", listOf(label("nsfw", 900)))
        store.flushIfDue()

        verifyNoInteractions(api)
    }

    @Test
    fun `a tripped guardian floor accumulates and flushes the category count`() = runBlocking {
        val (store, api, _) = make(ContentPolicyInputs(contentPolicy = floors(nsfw = "block"), contentNotify = true))

        store.record("post-1", listOf(label("nsfw", 700)))
        store.flushIfDue()

        val captor = ArgumentCaptor.forClass(List::class.java) as ArgumentCaptor<List<FfiFamilyContentNotice>>
        verify(api, timeout(1_000)).familyNotifyReport(capture(captor), org.mockito.ArgumentMatchers.anyInt())
        assertEquals(listOf(FfiFamilyContentNotice(category = "nsfw", count = 1u)), captor.value)
    }

    @Test
    fun `the same item's category counts once per local day, not per render`() = runBlocking {
        val (store, api, _) = make(ContentPolicyInputs(contentPolicy = floors(nsfw = "block"), contentNotify = true))

        // Same post, re-rendered three times (e.g. three snapshot ticks) — the
        // dedup set must count it once, not three (family-safety.md § Guardian
        // Notify: a re-render must never re-count).
        repeat(3) { store.record("post-1", listOf(label("nsfw", 700))) }
        store.flushIfDue()

        val captor = ArgumentCaptor.forClass(List::class.java) as ArgumentCaptor<List<FfiFamilyContentNotice>>
        verify(api, timeout(1_000)).familyNotifyReport(capture(captor), org.mockito.ArgumentMatchers.anyInt())
        assertEquals(listOf(FfiFamilyContentNotice(category = "nsfw", count = 1u)), captor.value)
    }

    @Test
    fun `an account switch drops pending counts and the dedup set`() {
        val (store, api, accountStores) = make(ContentPolicyInputs(contentPolicy = floors(nsfw = "block"), contentNotify = true))

        // Pull the real closer FamilyNotifyStore registered at construction
        // straight from Mockito's recorded invocation — the exact lambda
        // AccountStores.closeOpenStores() invokes on a real account switch or
        // sign-out (PhotoBackupEngine registers the same way). Not an
        // ArgumentCaptor: capture()/any() return null for a reference-typed
        // generic, and unlike familyNotifyReport's List<> param above,
        // registerCloser's non-null `() -> Unit` param NPEs on that null before
        // Mockito's matcher machinery resolves it.
        val registerCall = org.mockito.Mockito.mockingDetails(accountStores).invocations
            .single { it.method.name == "registerCloser" }
        @Suppress("UNCHECKED_CAST")
        val closer = registerCall.arguments[1] as () -> Unit

        store.record("post-1", listOf(label("nsfw", 700)))
        closer.invoke()
        store.flushIfDue()

        verifyNoInteractions(api)
    }
}
