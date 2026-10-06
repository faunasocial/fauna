package com.fauna.app.core

import android.content.pm.ApplicationInfo
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * **Android's cloud-backup exclusion claim must be TRUE of the manifest this
 * build actually ships.**
 *
 * The client-device custodian holds a full sealed copy of the owner's corpus on
 * this phone. `CustodianStore::ensure_root` will not hand out that store without
 * a stated [com.fauna.ffi.FfiCloudBackupExclusion], and android's arm is
 * declarative: [CloudBackupPosture.DECLARATION] tells shared Rust *"the manifest
 * disables OS backup"*. Rust cannot check that — it has no manifest — so the
 * claim is taken on trust at exactly the point where being wrong is
 * catastrophic and invisible: Google device backup would replicate the sealed
 * corpus into the same vendor cloud that holds the keychain with the seed that
 * opens it, and nothing on the phone would look wrong.
 *
 * So the claim gets a test rather than a comment. This is the headless half of
 * "split the mile" — nothing here needs an eye, because `allowBackup` is
 * observable at runtime.
 *
 * ## Why `applicationInfo.flags` and not the manifest source
 *
 * [ApplicationInfo.FLAG_ALLOW_BACKUP] is the **merged, resolved** truth the OS
 * itself reads — the same value a library manifest could flip back on through
 * the manifest merger without anyone editing
 * `apps/fauna-android/app/src/main/AndroidManifest.xml`. Grepping the source
 * file would miss exactly that case.
 *
 * The desktop twin of this claim is `cloud_backup_exclusion()` in
 * `bins/fauna-sync-agent/src/custodian.rs`, whose own tests pin that an
 * established platform states a posture and an unestablished one refuses.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class CloudBackupPostureTest {

    /**
     * The load-bearing half. If someone flips `android:allowBackup` to true (or
     * a merged library manifest does), this goes red — which is the only warning
     * that exists before a sealed corpus starts flowing into Google's cloud.
     */
    @Test
    fun theShippedManifestReallyDisablesOsBackup() {
        val info = ApplicationProvider
            .getApplicationContext<android.app.Application>()
            .applicationInfo
        assertEquals(
            "android:allowBackup must stay false: the custodian's sealed store lives in " +
                "app-private storage, and CloudBackupPosture.DECLARATION tells shared Rust " +
                "the manifest excludes it. Enabling OS backup makes that claim a lie and " +
                "replicates the owner's whole sealed corpus into the vendor cloud that also " +
                "holds the keychain opening it.",
            0,
            info.flags and ApplicationInfo.FLAG_ALLOW_BACKUP,
        )
    }

    /**
     * The other half: the declaration must name the rule this test checks.
     *
     * On its own the assertion above only proves *some* property of the manifest;
     * what makes it a check of the claim is that the claim says this. Reword
     * [CloudBackupPosture.DECLARATION] to assert something else and this fails,
     * forcing whoever changed it to bring a check for the new claim — which is
     * the whole point of the enum carrying a reviewable string instead of a bare
     * "nothing to do".
     */
    @Test
    fun theDeclarationNamesTheRuleThisTestChecks() {
        assertTrue(
            "the declaration handed to Rust must name the file and attribute a reviewer " +
                "(and the test above) can check; got: ${CloudBackupPosture.DECLARATION}",
            CloudBackupPosture.DECLARATION.contains("AndroidManifest.xml") &&
                CloudBackupPosture.DECLARATION.contains("android:allowBackup=\"false\""),
        )
    }

    /**
     * Android must hand Rust the **declarative** arm. The FFI enum has no
     * `NotApplicable` arm at all (that omission is deliberate —
     * `libs/fauna-ffi/src/custodian_host.rs` module docs), so the only wrong
     * answer reachable from here is apple's imperative one, which on android
     * would run a shell callback that excludes nothing.
     */
    @Test
    fun androidStatesTheDeclarativeArm() {
        assertTrue(
            "android's exclusion must be DeclaredInManifest — the manifest is the only " +
                "thing that decides Google device backup, and no runtime call substitutes",
            CloudBackupPosture.exclusion()
                is com.fauna.ffi.FfiCloudBackupExclusion.DeclaredInManifest,
        )
    }
}
