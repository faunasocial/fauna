package com.fauna.app.core

import com.fauna.ffi.DecodedPost
import com.fauna.ffi.DecodedReference
import uniffi.fauna_core.AuthoringOriginStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Pure-Kotlin unit tests for [toPostDetail] — the native `fauna.posts.get`
 * (`DecodedPost`) → unified [com.fauna.app.data.api.PostDetail] projection the
 * post-detail screen renders. The `DecodedPost`/`DecodedReference` records are
 * constructed directly (no FFI call, no `.so`), so this runs without Robolectric.
 */
class PostDetailMappingTest {

    private fun decoded(
        author: String = "aa".repeat(32),
        body: String = "hello",
        createdAtMicros: Long = 1_700_000_000_000_000L,
        tags: List<String> = emptyList(),
        references: List<DecodedReference> = emptyList(),
    ) = DecodedPost(
        postId = "bb".repeat(32),
        author = author,
        body = body,
        createdAt = createdAtMicros,
        tags = tags,
        valid = true,
        items = emptyList(),
        references = references,
        facets = emptyList(),
        contentWarning = null,
        authoringOrigin = AuthoringOriginStatus.UNKNOWN,
    )

    @Test
    fun mapsContentFieldsAndConvertsMicrosToMillis() {
        val d = decoded(
            body = "the body",
            tags = listOf("rust", "fauna"),
            createdAtMicros = 1_700_000_000_000_000L,
        )
        val pd = d.toPostDetail(navPostId = "navid", source = "fauna")

        assertEquals("navid", pd.postId) // navigated id is echoed, not the decoded one
        assertEquals(d.author, pd.author)
        assertEquals("the body", pd.body)
        assertEquals(listOf("rust", "fauna"), pd.tags)
        assertEquals("fauna", pd.source)
        // micros → millis: fauna_core::Timestamp is microseconds; without the /1000
        // the absolute-time formatter renders ~50,000 AD.
        assertEquals(1_700_000_000_000L, pd.createdAt)
        assertFalse(pd.isReply)
        assertEquals(0, pd.likeCount) // a content read carries no interaction counts
    }

    @Test
    fun replyReferenceSetsIsReplyAndReplyToId() {
        val d = decoded(
            references = listOf(
                DecodedReference(refType = "reply", postId = "cc".repeat(32), emoji = null),
            ),
        )
        val pd = d.toPostDetail(navPostId = "navid", source = "")

        assertTrue(pd.isReply)
        assertEquals("cc".repeat(32), pd.replyToId)
        assertEquals("fauna", pd.source) // blank source defaults to fauna
    }

    @Test
    fun nonReplyReferenceLeavesIsReplyFalse() {
        val d = decoded(
            references = listOf(
                DecodedReference(refType = "quote", postId = "dd".repeat(32), emoji = null),
            ),
        )
        val pd = d.toPostDetail(navPostId = "navid", source = "activitypub")

        assertFalse(pd.isReply)
        assertEquals(null, pd.replyToId)
        assertEquals("activitypub", pd.source) // explicit source preserved (ProtocolBadge)
    }
}
