package com.fauna.app.provider

import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiOnDemandPrefsStore
import java.io.File
import java.nio.file.Files
import java.nio.file.StandardCopyOption

/** The device-local "show on demand" toggle store (internal wiring, never a user choice). */
internal const val ON_DEMAND_PREFS_FILE = "on-demand-prefs.json"

/**
 * [successorHex]'s on-demand toggle store, carried across an identity
 * succession first (`sync-agent.md` § The per-actor scope follows the account
 * across a succession, the android clause). The store sits in each account's
 * own scope ([pathFor]), so a successor would otherwise open a fresh one and a
 * root turned off under the predecessor would read ON again.
 *
 * The carry is the agent's rule: when the successor's scope holds no store,
 * the nearest hop of [predecessorsHex] (the registry's `predecessors_of`,
 * nearest first) whose scope holds one is renamed into it — one hop, never a
 * copy or a merge; a successor store already present stays and the
 * predecessor's is left where it is. The predecessor-keyed entries are then
 * re-keyed by the shared `carryFromPredecessors`, which is idempotent, so a
 * crash between the rename and the re-key is finished on the next open.
 */
internal fun carriedOnDemandPrefs(
    successorHex: String,
    predecessorsHex: List<String>,
    pathFor: (String) -> File,
): FfiOnDemandPrefsStore {
    val target = pathFor(successorHex)
    if (predecessorsHex.isEmpty()) return FfiOnDemandPrefsStore(target.absolutePath)
    val source = predecessorsHex.asSequence().map(pathFor).firstOrNull { it.isFile }
    if (source != null) {
        if (target.exists()) {
            ShellLog.i(TAG, "successor already holds an on-demand store — the predecessor's stays where it is")
        } else {
            target.parentFile?.mkdirs()
            Files.move(source.toPath(), target.toPath(), StandardCopyOption.ATOMIC_MOVE)
            ShellLog.i(TAG, "on-demand store carried from a predecessor scope")
        }
    }
    val store = FfiOnDemandPrefsStore(target.absolutePath)
    runCatching { store.carryFromPredecessors(successorHex, predecessorsHex) }
        .onFailure { ShellLog.w(TAG, "on-demand toggle re-key failed: ${it.message}") }
    return store
}

private const val TAG = "OnDemandPrefsCarry"
