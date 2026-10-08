package com.fauna.app.testing

import android.os.Looper
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.material3.Text
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.snapshots.Snapshot
import java.time.Duration
import org.junit.Assert.assertFalse
import org.junit.FixMethodOrder
import org.junit.Test
import org.junit.runner.RunWith
import org.junit.runners.MethodSorters
import org.robolectric.Robolectric
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

/**
 * Pins [FaunaRobolectricTestRunner]'s main-looper drain at the test boundary.
 *
 * Compose applies a snapshot write made outside composition through a
 * process-global pump: the write wakes `GlobalSnapshotManager`, whose consumer
 * runs on `AndroidUiDispatcher.Main`, and that dispatcher posts ONE main-looper
 * message to drain its queue — then believes itself scheduled until that message
 * runs. Robolectric clears the main looper between tests. A test that ends with
 * the message still queued (any test that writes snapshot state — an `AppState`
 * field, say — and never idles the looper) therefore leaves the dispatcher
 * scheduled for a message that no longer exists: it never posts again, no later
 * test's outside-composition write is ever applied, and every Compose test after
 * it burns its 60 s idle budget in `AppNotIdleException` ("Compose did not get
 * idle"). Which test strands it moves with timing, so the full suite failed
 * deterministically by class count while every class passed alone.
 *
 * The two methods run in name order: the first strands a write exactly as those
 * tests did; the second needs the pump alive.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
@FixMethodOrder(MethodSorters.NAME_ASCENDING)
class ComposeDispatcherSurvivesTestBoundaryTest {

    private fun composeShowing(text: () -> String) {
        Robolectric.buildActivity(ComponentActivity::class.java).setup().get()
            .setContent { Text(text()) }
        shadowOf(Looper.getMainLooper()).idle()
    }

    @Test
    fun a_aTestThatEndsWithAnUnappliedWriteQueued() {
        val state = mutableStateOf("before")
        composeShowing { state.value }

        // Written outside composition and never idled: the apply pump's
        // main-looper message is still queued when this test returns.
        state.value = "after"
    }

    @Test
    fun b_theNextTestsOutsideCompositionWriteIsStillApplied() {
        val state = mutableStateOf("before")
        composeShowing { state.value }

        state.value = "after"
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(500))

        assertFalse(
            "the global snapshot write was never applied — the previous test " +
                "stranded Compose's main-thread dispatcher",
            Snapshot.current.hasPendingChanges(),
        )
    }
}
