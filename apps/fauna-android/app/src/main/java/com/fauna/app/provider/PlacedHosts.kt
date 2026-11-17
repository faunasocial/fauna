package com.fauna.app.provider

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.launch

/**
 * Build the live host of every set the presence plan places on this device —
 * the provider's own host, one per set (`p2p-shared-set-build.md` § Phone
 * peers — design, decision 1), so a second call finds it built. The process's
 * host registry reports only what the provider holds, and both the relay seat
 * (`relay-serving.md` § Relay serving → *Which seats hold*, android's provider
 * row) and the share plane read that registry: whatever they start on, they
 * build the hosts first. A host that fails to build is reported to
 * [onHostFailure] and the rest are still built.
 */
suspend fun OnDemandSource.buildPlacedHosts(
    refresh: Boolean,
    onHostFailure: (scopedId: String, error: Throwable) -> Unit,
) {
    for (set in sets(refresh)) {
        runCatching { host(set.scopedId) }.onFailure { onHostFailure(set.scopedId, it) }
    }
}

/**
 * The relay seat's start, in every flavor: the placed sets' hosts are built,
 * THEN [announce] runs — the announce names the sets whose hosts the process
 * holds, and on the store-safe and kids flavors no share plane builds them, so
 * without this order a cold-started store build would announce nothing until
 * the file browser opened a folder.
 */
suspend fun hostsThenAnnounce(
    source: OnDemandSource,
    onHostFailure: (scopedId: String, error: Throwable) -> Unit,
    announce: suspend () -> Unit,
) {
    source.buildPlacedHosts(refresh = false, onHostFailure)
    announce()
}

/**
 * A set the presence plan adds while a seat runs (a share accepted in front)
 * gets its host too: every re-plan tick on [reconciled] builds the host of each
 * id [desired] holds. [desired] must read the held plan, never re-plan — a read
 * that re-planned would emit again and feed its own trigger. The caller
 * cancels the returned job on its stop.
 */
fun OnDemandSource.watchPlacedHosts(
    scope: CoroutineScope,
    reconciled: Flow<Unit>,
    desired: () -> List<String>,
    onHostFailure: (scopedId: String, error: Throwable) -> Unit,
): Job = scope.launch {
    reconciled.collect {
        for (id in desired()) {
            runCatching { host(id) }.onFailure { onHostFailure(id, it) }
        }
    }
}
