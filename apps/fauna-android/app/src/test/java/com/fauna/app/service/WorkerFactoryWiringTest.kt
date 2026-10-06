package com.fauna.app.service

import androidx.work.CoroutineWorker
import com.fauna.app.widget.WidgetDataWorker
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Tripwire for a whole-class-of-features silent failure: **a worker WorkManager
 * cannot construct never runs, and looks exactly like a worker with nothing to
 * do.**
 *
 * Every worker this app enqueues is a [HiltWorker] whose `@AssistedInject`
 * constructor takes injected dependencies beyond `(Context, WorkerParameters)`.
 * WorkManager's *default* [androidx.work.WorkerFactory] builds a worker by
 * reflecting exactly that two-argument constructor, so under it every one of
 * these classes fails to construct and its work is marked failed — with no
 * crash, no log the user sees, and no effect. Only
 * [androidx.hilt.work.HiltWorkerFactory] can build them, which is why
 * `FaunaApp.workManagerConfiguration` installs it and the manifest removes
 * WorkManager's eager androidx.startup initializer (that initializer would
 * otherwise win and install the default factory).
 *
 * This test pins the *reason* that wiring must exist. It asserts the property
 * that makes the default factory insufficient — an `@HiltWorker` annotation with
 * no default-constructible shape — so that if someone later reverts
 * `setWorkerFactory` or restores the eager initializer, the invariant they broke
 * is written down right here rather than being discovered as "photo backup
 * mysteriously never runs".
 *
 * (The live construction path itself needs a real device or emulator, which the
 * android end-to-end lane does not have available yet. This is the headless
 * half; proving the enqueue -> construct -> run path belongs to that lane.)
 */
class WorkerFactoryWiringTest {

    private val enqueuedWorkers = listOf(
        PhotoBackupWorker::class.java,
        CustodianHostWorker::class.java,
        WidgetDataWorker::class.java,
    )

    /**
     * Note on what is *not* asserted here: `@HiltWorker` is not `RUNTIME`-retained,
     * so `isAnnotationPresent` cannot see it — an earlier version of this test
     * asserted it and failed for that reason, not because the annotation was
     * missing. The constructor shape below is the observable proxy, and it is
     * the property that actually matters anyway.
     */
    @Test
    fun everyEnqueuedWorkerIsACoroutineWorker() {
        for (worker in enqueuedWorkers) {
            assertTrue(
                "${worker.simpleName} must be a CoroutineWorker",
                CoroutineWorker::class.java.isAssignableFrom(worker),
            )
        }
    }

    /**
     * The load-bearing half: none of these has the bare
     * `(Context, WorkerParameters)` constructor WorkManager's default factory
     * reflects for. If one ever did, it would construct under the default
     * factory *without* its injected dependencies — which is why the fix is a
     * real factory rather than "add a two-arg constructor".
     */
    @Test
    fun noEnqueuedWorkerIsConstructibleByTheDefaultFactory() {
        for (worker in enqueuedWorkers) {
            val defaultConstructible = worker.constructors.any { ctor ->
                ctor.parameterTypes.size == 2 &&
                    ctor.parameterTypes[0] == android.content.Context::class.java &&
                    ctor.parameterTypes[1] == androidx.work.WorkerParameters::class.java
            }
            assertTrue(
                "${worker.simpleName} exposes a bare (Context, WorkerParameters) " +
                    "constructor: WorkManager's default factory would build it with " +
                    "no dependencies injected. Keep the @AssistedInject shape.",
                !defaultConstructible,
            )
        }
    }
}
