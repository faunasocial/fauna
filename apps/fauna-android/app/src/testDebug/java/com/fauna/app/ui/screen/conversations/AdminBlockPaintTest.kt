package com.fauna.app.ui.screen.conversations

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Which shape a DM bubble paints an admin block's notice in (`moderation.md` § Admin block of
 * a reported item, decision 4): in place of the bubble for a withheld record (empty document),
 * beside the body for the author's own blocked message, none when unblocked.
 */
class AdminBlockPaintTest {

    @Test
    fun aWithheldRecordPaintsTheNoticeInPlace() {
        assertEquals(AdminBlockPaint.IN_PLACE, adminBlockPaint("harassment", 0))
    }

    @Test
    fun theAuthorsOwnBlockedMessageKeepsItsBodyWithTheNoticeBeside() {
        assertEquals(AdminBlockPaint.BESIDE, adminBlockPaint("harassment", 2))
    }

    @Test
    fun anUnblockedMessagePaintsNoNotice() {
        assertEquals(AdminBlockPaint.NONE, adminBlockPaint(null, 0))
        assertEquals(AdminBlockPaint.NONE, adminBlockPaint(null, 3))
    }
}
