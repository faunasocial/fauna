package com.fauna.app.core

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * The surviving-device box read, shared by the two readers that need it —
 * [com.fauna.app.ui.screen.onboarding.NestRecoveryVM] and the launch-retry
 * surface's [com.fauna.app.ui.viewmodel.AppLaunchVM] (`box-recovery.md` § The
 * plane-era recovery floor, *(b) The reads*). Twin of apple's
 * `RecoverableBoxes.load`.
 *
 * With a stored nest URL: connect a short-lived [com.fauna.ffi.FfiNestClient]
 * and read [com.fauna.ffi.deploymentSeeds] — this device's own account store
 * joined with a cold read from that nest, in one shared call. When the nest
 * cannot be reached (the total-box-loss case, where the saved nest *is* the dead
 * box) or answers nothing, fall back to [com.fauna.ffi.deploymentSeedsLocal], so
 * a dead stored nest never hides what this device holds. With no nest URL, the
 * local read alone. Best-effort: any fault reads as an empty list.
 */
object RecoverableBoxes {
    suspend fun load(
        nestUrl: String,
        ownerSecret: ByteArray,
        storeContainerDir: String,
    ): List<String> = withContext(Dispatchers.IO) {
        // `appDataDir` is unused by the plane read (it rooted the retired
        // `__config` replica) — the store is located by its container.
        val local = { com.fauna.ffi.deploymentSeedsLocal(ownerSecret, "", storeContainerDir) }
        val seeds = if (nestUrl.isEmpty()) {
            runCatching { local() }
        } else {
            runCatching {
                val client = com.fauna.ffi.FfiNestClient(nestUrl, ownerSecret)
                try {
                    client.connect()
                    com.fauna.ffi.deploymentSeeds(client, ownerSecret, storeContainerDir)
                } finally {
                    client.disconnect()
                }
            }.onFailure {
                ShellLog.w("RecoverableBoxes", "nest read failed, reading this device's store alone: ${it.message}")
            }.mapCatching { it.ifEmpty { local() } }
                .recoverCatching { local() }
        }
        seeds.onFailure { ShellLog.w("RecoverableBoxes", "read failed (best-effort): ${it.message}") }
            .getOrDefault(emptyList())
            .map { it.nestActorId }
    }
}
