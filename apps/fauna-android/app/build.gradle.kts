import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
    id("com.google.devtools.ksp")
    id("com.google.dagger.hilt.android")
}

android {
    // Source-package namespace only (R class, BuildConfig, Kotlin packages).
    // Deliberately decoupled from applicationId — renaming every source file
    // buys nothing, while the store identity below is permanent once published.
    namespace = "com.fauna.app"
    compileSdk = 35

    defaultConfig {
        // The Play/store identity — on the org's own domain (fauna.social).
        // Permanent once the first artifact is uploaded to Play; never change
        // after that. The LEAF is the product name, lowercase — never a
        // platform word, because the registry already encodes the platform;
        // that rule is why this is `fauna` and not `android`, and why the Apple
        // bundle id and the linux/Flathub app id converge on the same string
        // without any store's rules being fought.
        // Owner: docs/goal/architecture/installers/android.md § Store identity.
        applicationId = "social.fauna.fauna"
        minSdk = 26
        targetSdk = 35
        // versionName is the product version — the ONLY number edited by hand.
        // versionCode is derived from it: major * 10000 + minor * 100 + patch,
        // so 0.1.0 -> 100 and 1.0.0 -> 10000. Play enforces versionCode alone
        // (it rejects both reuse and regression), and deriving it means a
        // product-version bump can never accidentally lower it. A reship of
        // unchanged code bumps the PATCH — Play demands a fresh versionCode for
        // every upload, and this scheme gives each product version exactly one.
        // Deliberately NOT computed from git history: F-Droid (a declared
        // channel) builds from a source tarball where that is not reproducible.
        // Owner: docs/goal/architecture/installers/android.md § Versioning.
        // versionName must equal [workspace.package] version in the root
        // Cargo.toml (product-version.md; version-lockstep merge gate).
        versionCode = 102
        versionName = "0.1.2"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    // Release signing — the Play **upload** key (installers/android.md § Signing
    // model). Google holds the app signing key that devices trust; this key only
    // authenticates uploads to the Console, and is resettable via Play support
    // if it is ever lost. So the keystore and its passwords live OUTSIDE the
    // repo — `~/.config/fauna/android/{upload-keystore.jks,signing.properties}`,
    // never a tracked file, never a commit body.
    //
    // Absent properties file → no signing config → release builds stay unsigned,
    // which is the state every fresh checkout and every development machine but
    // the one holding the key builds in. That is deliberate: an
    // unsigned `bundleRelease` still exercises the whole R8/minify path, so the
    // release build stays verifiable everywhere and only the final upload
    // artifact needs the key.
    val uploadSigningProps: Properties? =
        File(System.getProperty("user.home"), ".config/fauna/android/signing.properties")
            .takeIf { it.isFile }
            ?.let { f -> Properties().apply { f.inputStream().use { load(it) } } }

    signingConfigs {
        uploadSigningProps?.let { props ->
            create("upload") {
                storeFile = File(props.getProperty("storeFile"))
                storePassword = props.getProperty("storePassword")
                keyAlias = props.getProperty("keyAlias")
                keyPassword = props.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        debug {
            buildConfigField("boolean", "PAYMENTS", "true")
            buildConfigField("boolean", "P2P_SHARE", "true")
            buildConfigField("boolean", "KIDS", "false")
        }
        release {
            // Null when no signing.properties is present — an unsigned release,
            // the default everywhere but the key-holding machine (see above).
            signingConfig = signingConfigs.findByName("upload")
            isMinifyEnabled = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "../proguard-rules.pro"
            )
            buildConfigField("boolean", "PAYMENTS", "true")
            buildConfigField("boolean", "P2P_SHARE", "true")
            buildConfigField("boolean", "KIDS", "false")
        }
        // The App-Store escape hatch's android flavor (dynamic-features.md
        // § The App-Store escape hatch): `release`, minus every gated-feature
        // registry member. `initWith(release)` rather than a hand-copied block
        // so the two can never drift on minification, proguard files or
        // signing — the excised artifact must be the shipping one minus the
        // members and nothing else, which is also why the witness
        // (`just android-store-safe-check`) greps THIS variant and not a debug
        // build.
        //
        // Built by `just android-store-safe`, which stages a `fauna-ffi`
        // compiled `--no-default-features --features store-safe,file-provider-host` into
        // `src/storeSafe/` — so this variant's bindings carry no
        // `FfiPaymentsClient` at all and the `src/noPayments/` glue twin below
        // is what keeps it compiling.
        create("storeSafe") {
            initWith(getByName("release"))
            // Consumed by the render conditions in ProfileTiersTab /
            // SubscriptionSettingsScreen / FeedScreen's TipSurface. R8 folds a
            // `static final false` and strips the branch, which is how the
            // element ids leave the artifact — the glue's absence alone would
            // leave every `subscription-provider-*` / `subscription-claim-*` /
            // `post-tip-*` id in the dex as a dead string literal.
            buildConfigField("boolean", "PAYMENTS", "false")
            // The `p2p-share` member's condition, the same shape one member
            // over: consumed by FoldersScreen's co-present ceremony section and
            // its group rows, so the release shrinker folds the `offline-share-*` /
            // `offline-receive-*` ids out of this variant's dex.
            buildConfigField("boolean", "P2P_SHARE", "false")
            buildConfigField("boolean", "KIDS", "false")
            // Distinguishes the outputs so a store-safe APK can never be
            // mistaken for the shipping one on disk.
            versionNameSuffix = "-storesafe"
        }
        // The F-Droid / direct-download artifact (installers/android.md
        // § Release channels): `release` minus every proprietary Google Play
        // client library. The two Play libraries (Play Integrity + Play Age
        // Signals — the store-age arm of `family-safety.md` § The account age
        // band) are closed-source IPC shims to the Play Store app, which
        // F-Droid's inclusion policy forbids and a Play-less device cannot
        // serve; they are declared only on the Play-distributed build types
        // below, and this variant compiles the inert `src/noStoreAge/` twin
        // (the `src/noPayments/` posture, one plane over). Witness:
        // `just android-foss-check` greps the dex for `com.google.android.play`.
        create("foss") {
            initWith(getByName("release"))
            matchingFallbacks += listOf("release")
            buildConfigField("boolean", "PAYMENTS", "true")
            buildConfigField("boolean", "P2P_SHARE", "true")
            buildConfigField("boolean", "KIDS", "false")
            versionNameSuffix = "-foss"
        }
        // Fauna Kids (family-safety.md § The account age band, the kids-app
        // bullet; installers/android.md § Goal): `storeSafe` under its own
        // store identity. `initWith(storeSafe)` so the kids artifact is the
        // store-safe one minus the kids-excised planes and nothing else — the
        // same no-drift argument `storeSafe` makes against `release`.
        //
        // Built by `just android-kids`, which stages a `fauna-ffi` compiled
        // `--no-default-features --features kids-safe,kids-floor,file-provider-host`
        // into `src/kids/` — so this variant's bindings carry no feed, search,
        // bridge, web-publishing or catalog face at all, and the hand-written
        // twin beside them (`src/kids/java/com/fauna/app/`) is what keeps it
        // compiling.
        create("kids") {
            initWith(getByName("storeSafe"))
            matchingFallbacks += listOf("release")
            // Its store identity is set on the VARIANT, below this block
            // (`androidComponents.onVariants`): a build type can only suffix
            // the application id, and a suffix is exactly what the kids
            // listing must not be.
            // The render conditions' constant. R8 folds `static final true`
            // and strips every `if (!BuildConfig.KIDS)` branch, which is how
            // the excised surfaces' element ids leave the dex.
            buildConfigField("boolean", "KIDS", "true")
            // `initWith(storeSafe)` copied "-storesafe"; this artifact SHIPS,
            // on the train at the fleet version, so it carries no suffix.
            versionNameSuffix = null
        }
    }

    // The `payments` plane's glue lives in a per-build-type source set, because
    // Kotlin has no inline compile-time exclusion: `if (BuildConfig.PAYMENTS)`
    // is a runtime branch whose body must still typecheck, and a store-safe
    // build's generated bindings have no `FfiPaymentsClient` /
    // `FfiProviderItem` / `FfiClaimItem` / `FfiFeedManager.resolvePostTips` to
    // typecheck against. `src/payments` and `src/noPayments` hold
    // signature-identical twins (com.fauna.app.payments.PaymentsGlue) and are
    // the complete list of android code that may name a payments FFI symbol.
    //
    // androidTest inherits its buildType's set, so the instrumented suite sees
    // the same twin its variant does.
    // `src/noAgent` is the same shape for convention 15's automation surface,
    // and `storeSafe` is why it had to become a shared directory. The real
    // `TestAgent` lives in `src/debug`; its inert same-signature twin used to
    // sit in `src/release/java`, which works only while `release` is the sole
    // shipping flavor. `storeSafe` is a second one, and it cannot simply take
    // `src/release/java` — that directory also holds the release flavor's
    // staged UniFFI bindings, which would duplicate-declare against its own
    // (and re-introduce the payments face this whole build type exists to
    // remove). So the twin moved to its own directory that both shipping
    // flavors take. Without it, `assembleStoreSafe` fails on ~10 unresolved
    // `TestAgent` references from `src/main` — measured 2026-08-16, the first
    // store-safe assemble.
    // `src/storeAge` / `src/noStoreAge` are the same shape for the store-age
    // arm's Play half (`com.fauna.app.age.StoreAgeGlue`): the real twin on the
    // three Play-distributed build types, the inert twin on `foss`. They are
    // the complete list of android code that may name a
    // `com.google.android.play` symbol.
    // `src/p2pShare` / `src/noP2pShare` are the payments shape for the
    // `p2p-share` member's ceremony half
    // (`com.fauna.app.p2pshare.OfflineShareHost`): the built twin everywhere
    // but `storeSafe`, whose `fauna-ffi` exports no ceremony face. They are the
    // complete list of android code that may name a ceremony FFI symbol.
    // `src/noKids` / `src/kids` are the same shape for the Fauna Kids flavor,
    // one polarity over (family-safety.md § The account age band, the kids-app
    // bullet; dynamic-features.md § Compile-time excision): every shell file
    // that names a declaration the `kids-safe,kids-floor` bindings lack — the
    // feed, search, every bridge, web publishing, the subscriptions author
    // calls, connected apps, the labeler catalog — lives in `src/noKids`, taken
    // by the four other build types; `src/kids/java/com/fauna/app/` holds the
    // inert twin of only what `src/main` still names (`KidsExcisedApi`, the
    // `ApiClient` superclass; `KidsExcisedNav`; `CriticalAlertsHost`). `kids`
    // never takes `src/noKids`.
    sourceSets {
        getByName("debug") {
            java.srcDir("src/noKids/java")
            java.srcDir("src/payments/java")
            java.srcDir("src/p2pShare/java")
            java.srcDir("src/storeAge/java")
        }
        getByName("release") {
            java.srcDir("src/noKids/java")
            java.srcDir("src/payments/java")
            java.srcDir("src/p2pShare/java")
            java.srcDir("src/noAgent/java")
            java.srcDir("src/storeAge/java")
        }
        getByName("storeSafe") {
            java.srcDir("src/noKids/java")
            java.srcDir("src/noPayments/java")
            java.srcDir("src/noP2pShare/java")
            java.srcDir("src/noAgent/java")
            java.srcDir("src/storeAge/java")
        }
        getByName("foss") {
            java.srcDir("src/noKids/java")
            java.srcDir("src/payments/java")
            java.srcDir("src/p2pShare/java")
            java.srcDir("src/noAgent/java")
            java.srcDir("src/noStoreAge/java")
        }
        // `kids` is Play-distributed (Families policy), so it keeps the real
        // store-age twin; everything else is `storeSafe`'s set.
        getByName("kids") {
            java.srcDir("src/noPayments/java")
            java.srcDir("src/noP2pShare/java")
            java.srcDir("src/noAgent/java")
            java.srcDir("src/storeAge/java")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }

    kotlinOptions {
        jvmTarget = "21"
    }

    buildFeatures {
        compose = true
        // Gates the e2e TestAgent surface on BuildConfig.DEBUG (testing.md
        // convention 15) — R8 folds the constant-false branch in release and
        // strips the then-unreferenced TestAgent.start() body from the APK.
        buildConfig = true
    }

    testOptions {
        unitTests.isIncludeAndroidResources = true
        unitTests.all {
            it.jvmArgs("-Drobolectric.conscryptMode=OFF")
            // UniFFI bindings call into JNA at class-init; the committed
            // net.java.dev.jna:jna:5.14.0@aar ships only Android-ABI
            // dispatchers, so the host JVM needs the desktop jna jar (below)
            // plus this path to the host-built libfauna_ffi.so (memory
            // android-robolectric-ffi-needs-host-jna).
            System.getenv("CARGO_TARGET_DIR")?.let { td ->
                it.systemProperty("jna.library.path", "$td/debug")
            }
        }
    }
}

// The Fauna Kids store identity (installers/android.md § Store identity): a
// different registry unit, never a suffix of the main id, because a
// Families-policy listing is its own Play record. Freezes at its first upload.
// Set here rather than in the `kids` build type because AGP's build-type DSL
// offers only `applicationIdSuffix`; the variant's `applicationId` property is
// the one place a build type's artifact can take a whole different id.
androidComponents {
    onVariants(selector().withBuildType("kids")) { variant ->
        variant.applicationId.set("social.fauna.faunakids")
    }
}

// Room schema history — checked-in per-version JSON snapshots. Lets every
// migration be checked against Room's own generated
// DDL, and is the input `androidx.room.testing.MigrationTestHelper` needs to
// construct an "old" database and validate a migration against it on a real
// device/emulator (ARM64 dev machines can't execute a real SQLite query at
// all under Robolectric — its native shadow has no aarch64 build — so that
// proof is host-emulator-gated).
ksp {
    arg("room.schemaLocation", "$projectDir/schemas")
}

dependencies {
    // Compose
    val composeBom = platform("androidx.compose:compose-bom:2024.06.00")
    implementation(composeBom)
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")
    implementation("androidx.activity:activity-compose:1.9.0")
    implementation("androidx.navigation:navigation-compose:2.7.7")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.2")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.2")
    // ProcessLifecycleOwner — the app-foreground signal for the backup-custodian
    // foreground push-kick (com.fauna.app.service.CustodianPushKick).
    implementation("androidx.lifecycle:lifecycle-process:2.8.2")

    // Hilt
    implementation("com.google.dagger:hilt-android:2.51.1")
    ksp("com.google.dagger:hilt-compiler:2.51.1")
    implementation("androidx.hilt:hilt-navigation-compose:1.2.0")

    // Room
    implementation("androidx.room:room-runtime:2.6.1")
    implementation("androidx.room:room-ktx:2.6.1")
    ksp("androidx.room:room-compiler:2.6.1")

    // Networking
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.7.0")

    // Image loading
    implementation("io.coil-kt:coil-compose:2.6.0")

    // Google Play client libraries — the store-age arm of the account age band
    // (`family-safety.md` § The account age band, D3 + D5): Play Integrity
    // (classic request; the nest verifies the verdict locally) and Play Age
    // Signals (the store's age range). Proprietary Google IPC shims to the
    // Play Store app, so they are declared ONLY on the Play-distributed build
    // types — never on `foss` (the F-Droid / direct-download artifact), whose
    // `src/noStoreAge/` twin names no Play symbol. User-approved 2026-08-24
    // (dl.google.com/dl/android/maven2, the AndroidX channel).
    // (`storeSafe` is a custom build type, so its configuration has no
    // type-safe accessor — the string-invoke form names it.)
    debugImplementation("com.google.android.play:integrity:1.6.0")
    releaseImplementation("com.google.android.play:integrity:1.6.0")
    "storeSafeImplementation"("com.google.android.play:integrity:1.6.0")
    "kidsImplementation"("com.google.android.play:integrity:1.6.0")
    debugImplementation("com.google.android.play:age-signals:0.0.4")
    releaseImplementation("com.google.android.play:age-signals:0.0.4")
    "storeSafeImplementation"("com.google.android.play:age-signals:0.0.4")
    "kidsImplementation"("com.google.android.play:age-signals:0.0.4")

    // Emoji picker — the one sanctioned per-app divergence for the
    // dm-reaction-more-button "more" picker (conversations.md § Reactions &
    // message delete: native emoji picker where it exists; Android = emoji2
    // EmojiPickerView). Hosted via AndroidView in the stateful detail screen so
    // the FFI-free Compose Content / Robolectric harness never inflates it.
    implementation("androidx.emoji2:emoji2-emojipicker:1.4.0")

    // JNA (required by UniFFI-generated bindings)
    implementation("net.java.dev.jna:jna:5.14.0@aar")
    // Desktop jna jar for Robolectric on the host JVM — the @aar artifact
    // above ships only Android-ABI dispatchers (memory
    // android-robolectric-ffi-needs-host-jna). Test-only; never ships in the
    // APK.
    testImplementation("net.java.dev.jna:jna:5.14.0")

    // Security (EncryptedSharedPreferences)
    implementation("androidx.security:security-crypto:1.1.0-alpha06")

    // Biometric re-auth for the multi-account switch gate (long-term-store.md
    // § Multi-account evolution → Per-account re-auth; Stage 2). BiometricPrompt
    // requires a FragmentActivity host, so MainActivity extends FragmentActivity
    // (fragment-ktx pins a stack-aligned version rather than biometric's older
    // transitive one).
    implementation("androidx.biometric:biometric:1.1.0")
    implementation("androidx.fragment:fragment-ktx:1.8.1")

    // WorkManager + Hilt worker injection
    implementation("androidx.work:work-runtime-ktx:2.9.0")
    implementation("androidx.hilt:hilt-work:1.2.0")
    ksp("androidx.hilt:hilt-compiler:1.2.0")

    // Datastore (for simple preferences)
    implementation("androidx.datastore:datastore-preferences:1.1.1")

    // Glance (app widgets)
    implementation("androidx.glance:glance-appwidget:1.1.0")

    // Window size class (adaptive layout)
    implementation("androidx.compose.material3:material3-window-size-class")

    // Testing
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.8.1")
    testImplementation("org.mockito:mockito-core:5.12.0")
    androidTestImplementation(composeBom)
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")

    // Robolectric (JVM Android tests)
    testImplementation("org.robolectric:robolectric:4.13")
    testImplementation(composeBom)
    testImplementation("androidx.compose.ui:ui-test-junit4")
    testImplementation("androidx.test.ext:junit:1.1.5")
    debugImplementation("androidx.compose.ui:ui-test-manifest")

    // Instrumented tests
    androidTestImplementation("androidx.test.ext:junit:1.1.5")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.1")

    // E2E bridge (UIAutomator + HTTP server)
    androidTestImplementation("org.nanohttpd:nanohttpd:2.3.1")
    androidTestImplementation("androidx.test.uiautomator:uiautomator:2.3.0")
}
