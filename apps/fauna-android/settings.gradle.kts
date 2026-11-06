pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "Fauna"
include(":app")
// The out-of-process store driver (`store-driver/`; feature-catalog.md
// § Implementation status today, how the Android store install is driven) is
// included by the bump round that admits its dependencies — see the head of
// `store-driver/build.gradle.kts`.
