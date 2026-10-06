package com.fauna.app.core

import com.fauna.ffi.FfiCloudBackupExclusion

/**
 * How **android** keeps the client-device custodian's sealed store out of the
 * OS's own cloud backup — this platform's single statement of the obligation
 * `docs/goal/ui/backups.md` § Third destination kind (*Durability + labeling*)
 * puts on every host shell.
 *
 * The android twin of the desktop agent's `cloud_backup_exclusion()`
 * (`bins/fauna-sync-agent/src/custodian.rs`): **one** function per platform, so
 * the posture is stated once and reviewable at the call site rather than
 * re-asserted by each trigger. Both android hosts —
 * [com.fauna.app.service.CustodianHostWorker] and
 * [com.fauna.app.service.CustodianPushKick] — reach it through
 * [ApiClient.buildCustodianHost] and never name an exclusion themselves.
 *
 * ## Why declarative, and why the string
 *
 * `CustodianStore::ensure_root` takes no default and no "unknown" arm: a glue
 * author wiring a platform must say which arm is true. Android's is
 * **declarative** — `android:allowBackup="false"` in the manifest, which no
 * runtime call can substitute for (Google device backup is decided by the OS
 * from the manifest, before any app code runs). [DECLARATION] names the file and
 * the rule so a reviewer can *check* the claim rather than take it, and
 * [com.fauna.app.service.CloudBackupPostureTest] checks it mechanically against
 * the manifest this build actually shipped.
 *
 * The failure being prevented is invisible on the device: a full sealed corpus
 * replicated into the same vendor cloud that holds the keychain with the seed
 * that opens it, with nothing on the phone looking wrong. That is why the claim
 * gets a test rather than a comment.
 *
 * The FFI enum deliberately has **no** `NotApplicable` arm (`libs/fauna-ffi/src/
 * custodian_host.rs` module docs), so android cannot accidentally make the
 * desktop claim; the only thing it can say is this one, and it must be true.
 */
object CloudBackupPosture {

    /**
     * The manifest rule that excludes this app's private storage from Google
     * device backup — file plus attribute, in the form a reviewer greps for.
     */
    const val DECLARATION = "AndroidManifest.xml android:allowBackup=\"false\""

    /** Android's arm of the shared exclusion enum. */
    fun exclusion(): FfiCloudBackupExclusion =
        FfiCloudBackupExclusion.DeclaredInManifest(DECLARATION)
}
