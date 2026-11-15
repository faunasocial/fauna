package com.fauna.app.p2pshare

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import javax.inject.Inject
import javax.inject.Singleton

/**
 * The peer-transfer plane's host — **the excised half** (`dynamic-features.md`
 * § Platform-family surface excision), compiled into `storeSafe` and `kids` in
 * place of the `src/p2pShare/` twin. Signature-identical to it and naming no
 * plane FFI symbol: those bindings export no `startSharePlane`, no
 * `FfiSharePlaneListener` and no `sharePlaneView`, so a half-excised build is a
 * compile error, never a silent leak.
 *
 * Inert by design, not a dropped command (e2e convention 11's boundary): the
 * Folders page's transfer section carries the `BuildConfig.P2P_SHARE`
 * condition and [readings] stays `null` — "this device is not running the
 * plane", which renders nothing — and no setting or agent command reaches
 * [start].
 */
@Singleton
class SharePlaneHost @Inject constructor() : SharePlaneControl, SharePlaneReadingsSource {
    /** Always `null` — the plane never runs in this flavor. */
    override val readings: StateFlow<SharePlaneReadings?> = MutableStateFlow<SharePlaneReadings?>(null).asStateFlow()

    /** Excised — nothing to re-read. */
    fun refresh() = Unit

    /** Excised — no plane is started in this flavor. */
    override fun start() = Unit

    /** Excised — no plane runs, so none stops. */
    override fun stop() = Unit
}
