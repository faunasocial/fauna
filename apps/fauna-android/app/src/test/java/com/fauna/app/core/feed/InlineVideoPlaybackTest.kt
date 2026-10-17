package com.fauna.app.core.feed

import com.fauna.app.core.ApiClient
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiFeedManager
import com.fauna.ffi.FfiRangedNestReaderInterface
import java.io.File
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config
import uniffi.fauna_core.RenderBlock
import uniffi.fauna_feed.PlaybackSource

/**
 * The android leg of feed inline playback (`render-model.md` § D6c → *Inline playback*;
 * `security.md` § Transport trust → *The native-player leg*), pinned headlessly:
 * Robolectric cannot decode video, so what is asserted is the wiring — the phase
 * machine `video-thumbnail` publishes, that a `Url` source reaches the player as a
 * `MediaDataSource` over the pinned reader (never a URL), and that data source's read
 * rules over a fake reader. windows' `InlineVideoPlaybackTests` pins the same cursor
 * rules; the projection's own decision is pinned in shared Rust (`libs/fauna-feed`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class InlineVideoPlaybackTest {

    /** A resource of [len] bytes whose byte `i` is `i.toByte()`; [cap] narrows every
     *  answer, as a server answering less than the asked range would. */
    private class FakeReader(private val len: Long?, private val total: Long, private val cap: Long = Long.MAX_VALUE) :
        FfiRangedNestReaderInterface {
        val asks = mutableListOf<Pair<Long, Long>>()
        override fun contentType(): String? = "video/webm"
        override fun len(): ULong? = len?.toULong()
        override fun readAt(offset: ULong, maxLen: ULong): ByteArray {
            asks += offset.toLong() to maxLen.toLong()
            val end = minOf(total, offset.toLong() + minOf(maxLen.toLong(), cap))
            return ByteArray(maxOf(0L, end - offset.toLong()).toInt()) { (offset.toLong() + it).toByte() }
        }
    }

    @Test
    fun `a tap walks idle to loading, the prepared callback to playing`() {
        val state = VideoPlayerState()
        assertEquals(InlineVideoPlayback.Phase.IDLE, state.phase)
        val gen = state.tap()!!
        assertEquals(InlineVideoPlayback.Phase.LOADING, state.phase)
        assertNull("a tap while loading is the player's own, not a restart", state.tap())

        assertTrue(state.resolved(gen, InlineVideoPlayback.Media.Streamed("/api/v1/blob/aa", FakeReader(1, 1))))
        assertEquals("/api/v1/blob/aa", state.source)
        assertEquals(InlineVideoPlayback.Phase.LOADING, state.phase)
        state.prepared(gen)
        assertEquals(InlineVideoPlayback.Phase.PLAYING, state.phase)
        assertEquals("playing", state.phase.wire)
    }

    @Test
    fun `nothing playable or a player error is error, and a tap retries`() {
        val state = VideoPlayerState()
        val first = state.tap()!!
        state.resolved(first, null)
        assertEquals(InlineVideoPlayback.Phase.ERROR, state.phase)

        val second = state.tap()!!
        state.resolved(second, InlineVideoPlayback.Media.Streamed("/api/v1/blob/aa", FakeReader(1, 1)))
        state.failed(second)
        assertEquals(InlineVideoPlayback.Phase.ERROR, state.phase)
        assertFalse("a superseded attempt's late answer lands nowhere", state.resolved(first, null))
        state.prepared(first)
        assertEquals(InlineVideoPlayback.Phase.ERROR, state.phase)
    }

    @Test
    fun `a Url source opens the pinned reader on the login's nest URL, never a URL hand-off`() = runBlocking {
        val manager = mock(FfiFeedManager::class.java)
        val block = RenderBlock.Video(hash = "aa", alt = "")
        whenever(manager.playbackSource(block)).thenReturn(PlaybackSource.Url(url = "/api/v1/blob/aa"))
        val reader = FakeReader(10, 10)
        var opened: Pair<String, String>? = null
        val media = InlineVideoPlayback.resolve(
            manager, mock(ApiClient::class.java), block, "https://127.0.0.1:3000", File("/tmp"),
        ) { nestUrl, path ->
            opened = nestUrl to path
            reader
        }
        assertEquals("https://127.0.0.1:3000" to "/api/v1/blob/aa", opened)
        val streamed = media as InlineVideoPlayback.Media.Streamed
        assertSame(reader, streamed.reader)
        assertEquals("/api/v1/blob/aa", streamed.source)
    }

    @Test
    fun `an unplayable block or a refused reader resolves to nothing`() = runBlocking {
        val manager = mock(FfiFeedManager::class.java)
        val block = RenderBlock.Video(hash = "bb", alt = "")
        whenever(manager.playbackSource(block)).thenReturn(PlaybackSource.Unplayable(reason = "no"))
        assertNull(InlineVideoPlayback.resolve(manager, mock(ApiClient::class.java), block, "https://n", File("/tmp")))

        whenever(manager.playbackSource(block)).thenReturn(PlaybackSource.Url(url = "/api/v1/blob/bb"))
        assertNull(
            InlineVideoPlayback.resolve(manager, mock(ApiClient::class.java), block, "https://n", File("/tmp")) { _, _ ->
                throw IllegalStateException("unpinned cert")
            },
        )
    }

    @Test
    fun `the data source clamps to a known end and reads on past a short answer`() {
        val reader = FakeReader(len = 1000, total = 1000, cap = 100)
        val source = RangedNestDataSource(reader)
        assertEquals(1000L, source.size)
        val buf = ByteArray(600)
        assertEquals(500, source.readAt(500, buf, 100, 500))
        assertEquals((500).toByte(), buf[100])
        assertEquals(5, reader.asks.size)
        assertEquals("a read at the known end is the end, without a request", -1, source.readAt(1000, buf, 0, 10))
        assertEquals(5, reader.asks.size)
    }

    @Test
    fun `an unknown length reports size -1 and reads to the empty answer`() {
        val source = RangedNestDataSource(FakeReader(len = null, total = 10))
        assertEquals(-1L, source.size)
        val buf = ByteArray(32)
        assertEquals(10, source.readAt(0, buf, 0, 32))
        assertArrayEquals(ByteArray(10) { it.toByte() }, buf.copyOf(10))
        assertEquals(-1, source.readAt(10, buf, 0, 32))
    }
}
