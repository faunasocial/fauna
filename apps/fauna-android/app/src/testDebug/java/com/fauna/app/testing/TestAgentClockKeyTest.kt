package com.fauna.app.testing

import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config

/**
 * The `clock` state key — the wrong-clock launch witness's in-app control
 * (`test_onboarding_launch_routing_smoke.py` case L): the process that signed
 * in must be shown to run on the seeded clock, or a green silent challenge on
 * the real clock witnesses nothing. `fauna_e2e_agent::CLOCK_KEY` owns the shape,
 * `{"offset_secs", "now_secs"}`, identical on all seven apps.
 *
 * No offset is seeded in this JVM (the `FAUNA_E2E_CLOCK_OFFSET_SECS` door is
 * `MainActivity`'s `Os.setenv`, which a host test never runs), so the key must
 * read offset 0 and a `now` on the real clock — which proves both getters are
 * wired through to the shared launch clock, not stubbed. The offset arithmetic
 * itself is `fauna_launch_machine::launch_clock`'s own tests' to pin.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentClockKeyTest {

    @Test
    fun theClockKeyCarriesExactlyTheSharedShapeOffTheLaunchClock() {
        val clock = TestAgent.serializeState(AppState(), mock(SecureStorage::class.java), null)
            .getJSONObject("clock")

        assertEquals(
            "the shape is fauna_e2e_agent::clock_json's two keys, nothing more",
            setOf("offset_secs", "now_secs"),
            clock.keys().asSequence().toSet(),
        )
        assertEquals("no offset seeded in this process", 0L, clock.getLong("offset_secs"))
        // A value comparison with generous slack, never a wait (convention 14).
        val realNow = System.currentTimeMillis() / 1000
        val appNow = clock.getLong("now_secs")
        assertTrue(
            "now_secs must be the launch clock's epoch seconds: app=$appNow real=$realNow",
            kotlin.math.abs(appNow - realNow) < 600,
        )
    }
}
