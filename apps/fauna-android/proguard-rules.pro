# UniFFI-generated JNA bindings — do not rename, remove or optimize. Every
# package the bindgen writes, not only the `fauna-ffi` face: `com.fauna.ffi`
# plus one `uniffi.<crate>` package per shared crate the face re-exports
# (`fauna_core`, `fauna_launch_machine`, `fauna_client_capabilities`, …).
#
# Native code fills each package's JNA Structures (`RustBuffer`,
# `UniffiRustCallStatus`) through reflection, so the shrinker sees no write to their
# fields and folds them to their defaults. Measured 2026-10-08 on an unkept
# `uniffi.*` package: `RustBuffer.alloc()` "always" saw a null data pointer and
# threw, so `resolvedDialUrl` never returned, `ApiClient.authenticate` was
# compiled out of the launch path, and every secret-gated render — the Folders
# page's offline-share ceremony first — folded to its hidden branch.
-keep class com.fauna.ffi.** { *; }
-keep class uniffi.** { *; }

# JNA itself: libjnidispatch reaches its Java classes, fields (`Pointer.peer`)
# and methods by name over JNI, and reads every Structure subclass's fields by
# their declared `@FieldOrder` names.
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.Structure { *; }
# Kept whole, JNA's desktop-only `Native$AWT` helper names AWT, which Android
# has no copy of; nothing on Android reaches it.
-dontwarn java.awt.**

# kotlinx.serialization generated serializers
-keepclassmembers class * {
    kotlinx.serialization.KSerializer serializer(...);
}
-keep,includedescriptorclasses class com.fauna.app.data.api.**$$serializer { *; }
-keepclassmembers class com.fauna.app.data.api.** {
    *** Companion;
}
