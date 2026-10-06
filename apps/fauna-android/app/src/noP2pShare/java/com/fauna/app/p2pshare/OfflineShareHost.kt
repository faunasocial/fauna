package com.fauna.app.p2pshare

import android.content.Context
import com.fauna.app.core.ApiClient
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import javax.inject.Inject
import javax.inject.Singleton

/**
 * The co-present offline-share ceremony's host — **the excised half**
 * (`dynamic-features.md` § Platform-family surface excision, § The App-Store
 * escape hatch). Compiled into the `storeSafe` build type only, in place of
 * the `src/p2pShare/` twin.
 *
 * Signature-for-signature identical to that twin, and deliberately naming
 * **no ceremony FFI symbol at all** — a `storeSafe` build links a `fauna-ffi`
 * built `--no-default-features --features store-safe`, whose generated
 * Kotlin bindings carry no `FfiCeremonySeat`, no `FfiGroupShareViews` and no
 * `offlineShare*` function. That absence is the point: it makes a
 * half-excised build a **compile error** rather than a silent leak, so this
 * file failing to compile is the escape hatch working.
 *
 * **These are inert, not silently-dropped commands.** e2e convention 11
 * forbids an agent quietly swallowing a command, and this is the deliberate
 * exception the convention's own boundary allows: nothing can call the acts,
 * because the Folders page's ceremony section and group rows carry the
 * `BuildConfig.P2P_SHARE` condition and are not rendered in this flavor, and
 * [decision] answers `null` (the "hide the section" answer) besides. There is
 * no runtime flag, hidden setting or agent command that reaches them —
 * criterion 5 ("no re-enable path") is what makes an empty body the honest
 * implementation instead of an error.
 */
@Suppress("UNUSED_PARAMETER")
@Singleton
class OfflineShareHost @Inject constructor(
    @ApplicationContext private val appContext: Context,
) {
    /** Never bumps — there is no ceremony state in this flavor. */
    val changes: StateFlow<Int> = MutableStateFlow(0).asStateFlow()

    /** Always empty — no offered set can reach this flavor. */
    val groupShares: StateFlow<GroupShareRows> = MutableStateFlow(GroupShareRows()).asStateFlow()

    /** Always `null` — no act runs, so none stops short. */
    val error: StateFlow<String?> = MutableStateFlow<String?>(null).asStateFlow()

    /** Excised — `null` hides the section, as with no usable identity. */
    fun decision(api: ApiClient): OfflineShareDecision? = null

    /** Excised — the entry buttons are not rendered in this flavor. */
    fun open(api: ApiClient, which: OfflinePanel) = Unit

    /** Excised — the peer-code input is not rendered in this flavor. */
    fun setPeerCode(text: String) = Unit

    /** Excised — the begin button is not rendered in this flavor. */
    fun begin(api: ApiClient) = Unit

    /** Excised — the expect button is not rendered in this flavor. */
    fun expect(api: ApiClient) = Unit

    /** Excised — the cancel button is not rendered in this flavor. */
    fun cancel() = Unit

    /** Excised — no consent card is rendered in this flavor. */
    fun consent(api: ApiClient, scopeId: ByteArray) = Unit

    /** Excised — no consent card is rendered in this flavor. */
    fun decline(api: ApiClient, scopeId: ByteArray) = Unit

    /** Excised — there is no record to read. */
    fun loadGroupShares(api: ApiClient) = Unit

    /** Nothing to tear down: no seat is ever bound in this flavor. */
    fun reset() = Unit
}
