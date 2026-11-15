package com.fauna.app.p2pshare

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The android shell's lifecycle contract for the share plane
 * (`p2p-shared-set-build.md` § Phone peers — design, decision 2): start in
 * front once connected, stop on the background edge, and an identity change
 * stops the outgoing actor's plane and starts the incoming one's.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SharePlaneEdgesTest {

    private val scopes = mutableListOf<CoroutineScope>()

    @After
    fun cancelEdgeLoops() = scopes.forEach { it.cancel() }

    private class FakePlane : SharePlaneControl {
        val calls = mutableListOf<String>()
        override fun start() { calls += "start" }
        override fun stop() { calls += "stop" }
    }

    private fun edges(plane: FakePlane, connected: MutableStateFlow<Boolean>, scope: CoroutineScope) =
        SharePlaneEdges(plane, awaitConnected = { connected.first { it } }, scope = scope)

    /** A scope on the test's own scheduler, so `advanceUntilIdle` drives the edge loop. */
    private fun TestScope.edgeScope() = CoroutineScope(SupervisorJob() + StandardTestDispatcher(testScheduler))

    @Test
    fun theForegroundStartsThePlaneOnlyOnceTheClientIsConnected() = runTest {
        val plane = FakePlane()
        val connected = MutableStateFlow(false)
        val e = edges(plane, connected, edgeScope().also { scopes += it })

        e.foreground()
        advanceUntilIdle()
        assertEquals(emptyList<String>(), plane.calls)

        connected.value = true
        advanceUntilIdle()
        assertEquals(listOf("start"), plane.calls)
    }

    @Test
    fun theBackgroundEdgeStopsThePlaneAndTheNextForegroundStartsItAgain() = runTest {
        val plane = FakePlane()
        val e = edges(plane, MutableStateFlow(true), edgeScope().also { scopes += it })

        e.foreground(); advanceUntilIdle()
        e.background(); advanceUntilIdle()
        e.foreground(); advanceUntilIdle()

        assertEquals(listOf("start", "stop", "start"), plane.calls)
    }

    @Test
    fun aRepeatedForegroundEdgeStartsNoSecondPlane() = runTest {
        val plane = FakePlane()
        val e = edges(plane, MutableStateFlow(true), edgeScope().also { scopes += it })

        e.foreground(); e.foreground(); advanceUntilIdle()

        assertEquals(listOf("start"), plane.calls)
    }

    @Test
    fun anIdentityChangeInFrontStopsTheOutgoingPlaneAndStartsTheIncomingOne() = runTest {
        val plane = FakePlane()
        val connected = MutableStateFlow(true)
        val e = edges(plane, connected, edgeScope().also { scopes += it })
        e.foreground(); advanceUntilIdle()

        // The switch: the outgoing client goes, the incoming one connects later.
        connected.value = false
        e.identityChanged(); advanceUntilIdle()
        assertEquals(listOf("start", "stop"), plane.calls)

        connected.value = true
        advanceUntilIdle()
        assertEquals(listOf("start", "stop", "start"), plane.calls)
    }

    /** The relay seat rides the plane's edges: both start in front, both stop on the background edge. */
    @Test
    fun theRelaySeatStartsAndStopsOnThePlanesEdges() = runTest {
        val plane = FakePlane()
        val relay = FakePlane()
        val e = SharePlaneEdges(
            SharePlaneControls(listOf(plane, relay)),
            awaitConnected = { MutableStateFlow(true).first { it } },
            scope = edgeScope().also { scopes += it },
        )

        e.foreground(); advanceUntilIdle()
        assertEquals(listOf("start"), relay.calls)
        e.background(); advanceUntilIdle()
        e.identityChanged(); advanceUntilIdle()

        assertEquals(listOf("start", "stop", "stop"), plane.calls)
        assertEquals(listOf("start", "stop", "stop"), relay.calls)
    }

    @Test
    fun anIdentityChangeInTheBackgroundStartsNothing() = runTest {
        val plane = FakePlane()
        val e = edges(plane, MutableStateFlow(true), edgeScope().also { scopes += it })

        e.identityChanged(); advanceUntilIdle()

        assertEquals(listOf("stop"), plane.calls)
    }
}
