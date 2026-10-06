package com.fauna.app.testing

import org.junit.runners.model.FrameworkMethod
import org.robolectric.RobolectricTestRunner
import org.robolectric.internal.bytecode.InstrumentationConfiguration

/**
 * The Robolectric runner **every** Robolectric test in this module must use, in
 * place of [RobolectricTestRunner] itself.
 *
 * ## Why: UniFFI handles are process-global, Robolectric sandboxes are not
 *
 * Robolectric loads test classes in a per-sandbox `ClassLoader` so static state
 * cannot leak between tests. That isolation is exactly wrong for the generated
 * UniFFI bindings, because the state they keep in statics is not really
 * per-process Java state — it is a namespace **shared with Rust**:
 *
 *  * `FfiConverterType*.handleMap` is the only way Rust can find a Kotlin object
 *    it holds a handle for, and
 *  * `UniffiLib.INSTANCE`'s initialisation registers the callback vtables into
 *    Rust's globals, **last writer winning**.
 *
 * Load the bindings a second time in a sandbox and Rust ends up dispatching
 * callbacks into *that* copy's handle map, while the plain-JUnit tests keep
 * inserting into the application class loader's copy. Rust then passes back a
 * perfectly valid handle that the map it reaches has simply never heard of.
 *
 * The resulting failure is remote from its cause and destroys its own evidence:
 * the generated callback evaluates `handleMap.get(uniffiHandle)` **outside**
 * `uniffiTraitInterfaceCall`'s try/catch, so `InternalException("UniffiHandleMap
 * .get: Invalid handle")` escapes into JNA, which logs a warning and returns
 * without touching the out-params — leaving the call status at its
 * zero-initialised *success* code and the return buffer empty. Rust lifts
 * `Option<String>` from zero bytes and reports `Callback interface failure:
 * UnexpectedUniFFICallbackError(reason: "not enough bytes remaining in buffer
 * (0 < 1)")`, a wire-format message naming neither handles nor class loaders.
 *
 * Because which copy registers last depends on load order, and load order moves
 * with allocation and GC timing, this presented as an order-dependent flake:
 * it was originally bisected to "`core.*` plus `WorkerFactoryWiringTest`", a
 * test that only reflects over constructors and never touches the FFI at all.
 *
 * ## The fix
 *
 * Share the bindings and JNA with the parent class loader rather than copying
 * them per sandbox, so there is exactly one handle map and one vtable
 * registration — which is also what the app has on a real device. These
 * packages contain no Android API references (plain JVM + JNA), so none of them
 * needs Robolectric's instrumentation.
 *
 * Pinned by [com.fauna.app.core.FfiBindingsSingleHandleMapTest].
 */
class FaunaRobolectricTestRunner(testClass: Class<*>) : RobolectricTestRunner(testClass) {

    override fun createClassLoaderConfig(method: FrameworkMethod): InstrumentationConfiguration =
        InstrumentationConfiguration.Builder(super.createClassLoaderConfig(method))
            .doNotAcquirePackage(FFI_BINDINGS_PACKAGE)
            .doNotAcquirePackage(UNIFFI_MODULES_PACKAGE)
            .doNotAcquirePackage(JNA_PACKAGE)
            .build()

    companion object {
        /** Hand-written FFI surface + the generated `fauna_ffi.kt` bindings. */
        const val FFI_BINDINGS_PACKAGE = "com.fauna.ffi."

        /** Generated per-crate UniFFI modules (`uniffi.fauna_core`, …). */
        const val UNIFFI_MODULES_PACKAGE = "uniffi."

        /**
         * JNA owns the native callback trampolines the vtable is built from; a
         * second copy would load the library and register its own.
         */
        const val JNA_PACKAGE = "com.sun.jna."
    }
}
