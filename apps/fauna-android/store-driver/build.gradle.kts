// The out-of-process store driver: a self-instrumenting test APK that reaches
// the Fauna app only through `UiAutomation`, so it can drive an install the
// Play Store delivered — one re-signed with Google's app signing key, which the
// harness bridge in the app's own test APK can never instrument (Android
// attaches an instrumentation only to a package signed with its own
// certificate). Ruling: docs/goal/architecture/feature-catalog.md
// § Implementation status today, how the Android store install is driven.
//
// Self-instrumenting: its `<instrumentation>` targets its OWN package
// (src/main/AndroidManifest.xml), signed with the default debug key — a key of
// its own, never the app's. It has none of the harness bridge's same-UID
// routes: no credential seed, no log, download, clipboard or input-file route,
// no launch-environment seeds. Its dependencies are only the two the app's
// androidTest already carries (UiAutomator, and the NanoHTTPD the bridges
// serve over); a new artifact here waits for a dependency round, never an add.
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.fauna.storedriver"
    compileSdk = 36

    defaultConfig {
        // Never the app's id, nor `<app id>.test`: a package of its own, so it
        // installs beside a store install and shares nothing with it.
        applicationId = "social.fauna.storedriver"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "1"
    }

    sourceSets {
        // The find/actuate half of the UiAutomator bridge, lifted out of the
        // harness bridge into a source set both compile.
        getByName("main") {
            java.srcDir("../automation-shared/java")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }

    kotlinOptions {
        jvmTarget = "21"
    }
}

// ⚠ NO active `dependencies` block yet, and `settings.gradle.kts` does not
// include this module yet: its two dependency declarations are queued for the
// owner's bump round (`supply-chain/bump-queue.toml`). The block below is
// INERT: every line commented out between the two `bump-round inert` marker
// lines. The round's resolve step for either need activates the
// whole block (markers dropped, each line's `// ` removed, the need's
// coordinate written at its one version) and writes the
// `include(":store-driver")` line into `settings.gradle.kts`; the other
// need's resolve then finds its line already written (nothing moved). Never
// edit the block's coordinates by hand: a moved coordinate is a dependency
// step (dependency-bump-report.md § Implementation status today, a Gradle add
// into a module). Until the round activates it nothing configures or builds
// this module, so the app's own `assembleDebug` is untouched. Measured to
// build green 2026-11-03 against the existing
// `gradle/verification-metadata.xml` (no new artifact).
// bump-round inert: begin
// dependencies {
//     implementation("androidx.test.uiautomator:uiautomator:2.3.0")
//     implementation("org.nanohttpd:nanohttpd:2.3.1")
//     // This small graph resolved to exactly the versions the app's
//     // `debugAndroidTestRuntimeClasspath` resolves (read off
//     // `:app:dependencies --configuration debugAndroidTestRuntimeClasspath`);
//     // left alone, UiAutomator's own floor resolves annotation 1.4.0, which
//     // the verified set does not hold.
//     constraints {
//         implementation("androidx.annotation:annotation") { version { strictly("1.8.0") } }
//         implementation("androidx.annotation:annotation-jvm") { version { strictly("1.8.2") } }
//         implementation("androidx.tracing:tracing") { version { strictly("1.0.0") } }
//         implementation("org.jetbrains.kotlin:kotlin-stdlib") { version { strictly("2.0.21") } }
//     }
// }
// bump-round inert: end
