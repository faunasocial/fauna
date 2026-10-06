package com.fauna.app

import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * The headless witness for the slice-5 flip's **android arm**: this app no
 * longer ships an in-app segment-backup upload driver
 * (`docs/goal/behavior/backup-restore.md` § Background Tasks → *Flip status
 * (slice 5)*; the writer is the source nest —
 * `docs/goal/architecture/message-segment-store.md` § Cross-location backup
 * protocol).
 *
 * It needs no emulator, which matters because the e2e leg for `--app android`
 * is host-gated fleet-wide: without it, the arm's only evidence would be "the
 * code was deleted", and a driver coming back is invisible to a reviewer
 * reading a diff.
 */
class RetiredSegmentBackupDriverTest {

    /**
     * **The driver stays deleted.** The `client_runnable: false` flip in
     * `fauna_core::delegation::LIVE_TASK_KINDS` is gated on windows, apple *and*
     * android having no in-app driver, so re-adding one here would silently
     * invalidate that gate's premise rather than fail anything. The tempting way
     * to re-add it is named in the queue block that owns this arm: Settings → Task
     * delegation shows `backup-upload` with no *client* runner, and the row is
     * meant to show the **nest**, so do not put a driver back to make it look
     * right.
     */
    @Test
    fun noInAppSegmentBackupUploadDriverClassIsShipped() {
        for (name in listOf(
            "com.fauna.app.service.MailBackupWorker",
            "com.fauna.app.service.MailBackupPushKick",
        )) {
            try {
                Class.forName(name)
                fail(
                    "$name is back. The source nest is the segment-backup writer; an " +
                        "in-app upload driver can only double-write or diverge, and its " +
                        "absence is what gates the LIVE_TASK_KINDS client_runnable flip.",
                )
            } catch (expected: ClassNotFoundException) {
                assertTrue(true)
            }
        }
    }
}
