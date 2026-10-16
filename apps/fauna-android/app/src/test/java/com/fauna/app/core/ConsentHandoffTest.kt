package com.fauna.app.core

import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The `fauna://consent` route's one door on android ([ConsentHandoff];
 * `docs/goal/architecture/apps/android.md` § Navigation → *In-app routes*). The
 * parse is the real shared `parseAppRoute` over JNA, so a door that grew a
 * second parser — or dropped a route the shared grammar takes — fails here.
 * Twin of FaunaKit's `ConsentHandoffTests`.
 */
class ConsentHandoffTest {

    private val requestUri =
        "urn:ietf:params:oauth:request_uri:handoff-fixture"
    private val route = "fauna://consent/$requestUri"

    private fun signedIn() = AppState().apply { isOnboarding = false }

    @Test
    fun aConsentRouteSignedInIsStagedForThePage() {
        val door = ConsentHandoff()
        assertTrue(door.receive(route, authenticated = true))
        assertEquals(requestUri, door.pending.value)
        assertNull(door.held)
    }

    @Test
    fun aConsentRouteSignedOutIsHeldUntilTheSessionIsAuthenticated() {
        val door = ConsentHandoff()
        assertTrue(door.offer(route, AppState()))
        assertEquals(requestUri, door.held)
        assertNull("nothing is staged while signed out", door.pending.value)

        assertTrue(door.takeHeld())
        assertNull(door.held)
        assertEquals(requestUri, door.pending.value)
        assertFalse("a held route applies once", door.takeHeld())
    }

    @Test
    fun theAddAccountOverlayCountsAsSignedOut() {
        val door = ConsentHandoff()
        val appState = signedIn().apply { isAddingAccount = true }
        assertTrue(door.offer(route, appState))
        assertEquals(requestUri, door.held)
    }

    @Test
    fun anUnparseableOrForeignUriIsDroppedNeverHeld() {
        val door = ConsentHandoff()
        for (uri in listOf(
            "fauna://consent/",
            "fauna://consent/https://evil.example/",
            "fauna://consent/$requestUri/extra",
            "fauna://share-link?folder=42&path=photos/a.jpg",
            "https://example.com/",
            "not a uri",
        )) {
            assertFalse(uri, door.offer(uri, signedIn()))
        }
        assertNull(door.held)
        assertNull(door.pending.value)
    }

    @Test
    fun finishOpenClearsOnlyTheRequestThatWasOpened() {
        val door = ConsentHandoff()
        door.receive(route, authenticated = true)
        door.finishOpen("urn:ietf:params:oauth:request_uri:another")
        assertEquals("a newer route staged meanwhile stays pending", requestUri, door.pending.value)
        door.finishOpen(requestUri)
        assertNull(door.pending.value)
    }

    @Test
    fun aWaiterReturnsOnceThePageOpenFinishes() = runBlocking {
        val door = ConsentHandoff()
        door.receive(route, authenticated = true)
        assertFalse("nothing finishes the open → the deadline answers", door.waitUntilDrained(50))
        val waiter = async { door.waitUntilDrained(5_000) }
        door.finishOpen(requestUri)
        assertTrue(waiter.await())
    }
}
