package com.fauna.app.core

import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertSame
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Pins the invariant that keeps UniFFI handles meaningful in a JVM that runs
 * Robolectric and plain JUnit tests side by side: **the generated bindings
 * resolve to one class per process, so there is one handle map and one callback
 * vtable registration.**
 *
 * A second copy in a Robolectric sandbox gives Rust somewhere else to dispatch
 * callbacks, and a handle minted by the application class loader's map is then
 * unknown to the map Rust reaches — surfacing far away, in an unrelated plain
 * test, as `UnexpectedUniFFICallbackError(reason: "not enough bytes remaining in
 * buffer (0 < 1)")`. Full mechanism in [FaunaRobolectricTestRunner]'s KDoc.
 *
 * If this fails, a Robolectric test is using `RobolectricTestRunner` directly
 * instead of [FaunaRobolectricTestRunner], or a new generated-binding package
 * was added without being shared with the parent loader.
 */
@RunWith(FaunaRobolectricTestRunner::class)
class FfiBindingsSingleHandleMapTest {

    @Test
    fun bindingsResolveToTheSameClassInsideAndOutsideTheSandbox() {
        val sandboxLoader = this::class.java.classLoader
        val appLoader = ClassLoader.getSystemClassLoader()

        // Guard the guard: if this test ever stops running in a sandbox, the
        // comparisons below would hold trivially and pin nothing.
        assertNotSame(
            "This test must run inside a Robolectric sandbox for the rest to mean " +
                "anything, but its own loader is the application loader.",
            appLoader,
            sandboxLoader,
        )

        for (name in SHARED_BINDING_CLASSES) {
            val fromSandbox = Class.forName(name, false, sandboxLoader)
            val fromApp = Class.forName(name, false, appLoader)
            assertSame(
                "$name resolves to a different class inside the Robolectric sandbox " +
                    "than in the application class loader. That second copy has its own " +
                    "UniFFI handle map and re-registers the callback vtables, so Rust " +
                    "dispatches handles into a map that never saw them. Robolectric tests " +
                    "must run under FaunaRobolectricTestRunner.",
                fromApp,
                fromSandbox,
            )
        }
    }

    private companion object {
        /** The handle map, a generated module, and JNA's trampoline owner. */
        val SHARED_BINDING_CLASSES = listOf(
            "com.fauna.ffi.FfiConverterTypeFfiSecretStore",
            "com.fauna.ffi.UniffiLib",
            "uniffi.fauna_launch_machine.LaunchMachine",
            "com.sun.jna.Native",
        )
    }
}
