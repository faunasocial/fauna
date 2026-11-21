package com.fauna.app.provider

import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The relay seat's start in every flavor (`relay-serving.md` § Relay serving →
 * *Which seats hold*, android's provider row): the seat announces the sets
 * whose hosts the process holds, so it builds the host of every set the plan
 * places on this device BEFORE it announces — on the store-safe and kids
 * flavors no share plane builds them, and without this a cold-started store
 * build serves nothing until the file browser opens the folder.
 */
class PlacedHostsTest {

    private class FakeSource(
        private val placed: List<String>,
        private val failing: Set<String> = emptySet(),
        val calls: MutableList<String>,
    ) : OnDemandSource {
        override fun activeActorHex(): String = "ab"
        override suspend fun sets(refresh: Boolean): List<OnDemandSet> {
            calls += "sets(refresh=$refresh)"
            return placed.map { OnDemandSet(scopedId = it, name = it) }
        }
        override suspend fun host(scopedId: String): OnDemandSetHost? {
            calls += "host:$scopedId"
            if (scopedId in failing) throw IllegalStateException("no host")
            return null
        }
        override fun liveHosts(): Map<String, OnDemandSetHost> = emptyMap()
        override fun onTeardown(listener: () -> Unit) = Unit
    }

    @Test
    fun theSeatBuildsEveryPlacedSetsHostBeforeItAnnounces() = runTest {
        val calls = mutableListOf<String>()
        val source = FakeSource(listOf("local%3A1@ab", "local%3A2@ab"), calls = calls)

        hostsThenAnnounce(source, onHostFailure = { _, _ -> }) { calls += "announce" }

        assertEquals(
            listOf("sets(refresh=false)", "host:local%3A1@ab", "host:local%3A2@ab", "announce"),
            calls,
        )
    }

    @Test
    fun aHostThatFailsToBuildIsReportedAndTheSeatStillAnnouncesTheRest() = runTest {
        val calls = mutableListOf<String>()
        val failed = mutableListOf<String>()
        val source = FakeSource(listOf("a@ab", "b@ab"), failing = setOf("a@ab"), calls = calls)

        hostsThenAnnounce(source, onHostFailure = { id, _ -> failed += id }) { calls += "announce" }

        assertEquals(listOf("a@ab"), failed)
        assertEquals(listOf("sets(refresh=false)", "host:a@ab", "host:b@ab", "announce"), calls)
    }

    @Test
    fun aReconcileTickAfterStartBuildsTheNewSetsHost() = runTest {
        val calls = mutableListOf<String>()
        val source = FakeSource(emptyList(), calls = calls)
        val ticks = MutableSharedFlow<Unit>()
        var plan = listOf("a@ab")

        val job = source.watchPlacedHosts(this, ticks, { plan }, { _, _ -> })
        runCurrent()
        ticks.emit(Unit)
        plan = listOf("a@ab", "b@ab")
        ticks.emit(Unit)
        job.cancel()

        assertEquals(listOf("host:a@ab", "host:a@ab", "host:b@ab"), calls)
    }
}
