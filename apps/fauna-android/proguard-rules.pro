# UniFFI-generated JNI bindings — do not rename or remove
-keep class com.fauna.ffi.** { *; }

# kotlinx.serialization generated serializers
-keepclassmembers class * {
    kotlinx.serialization.KSerializer serializer(...);
}
-keep,includedescriptorclasses class com.fauna.app.data.api.**$$serializer { *; }
-keepclassmembers class com.fauna.app.data.api.** {
    *** Companion;
}
