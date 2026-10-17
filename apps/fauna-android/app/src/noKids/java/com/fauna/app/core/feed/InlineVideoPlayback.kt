package com.fauna.app.core.feed

import android.media.MediaDataSource
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiFeedManager
import com.fauna.ffi.FfiRangedNestReader
import com.fauna.ffi.FfiRangedNestReaderInterface
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.fauna_core.RenderBlock
import uniffi.fauna_feed.PlaybackSource

/**
 * The android half of feed inline playback — `video-thumbnail` as the player host
 * (`docs/goal/architecture/render-model.md` § D6c → *Inline playback*, answer (1)):
 * the shared `playbackSource` projection decides what plays, the framework
 * `MediaPlayer` plays it. No Media3 / ExoPlayer — zero new dependencies.
 *
 * **What the player reads.** A `Url` source never reaches the player as a URL
 * (`security.md` § Transport trust → *The native-player leg*): `MediaPlayer` handed
 * a URI dials the nest through the OS media stack, which trusts the OS store alone
 * — no loopback carve-out, no SPKI pin — and refuses every nest on its self-signed
 * floor. So it reads [RangedNestDataSource], a `MediaDataSource` over the shared
 * pinned `FfiRangedNestReader`. A `Sealed` source (a video of a post this reader
 * unlocked) is fetched whole, opened through [PostMediaOpen] like a sealed photo, and
 * played from a cache-dir temp file.
 */
object InlineVideoPlayback {
    /** What a tap resolved to. */
    sealed interface Media {
        /** The [source] the element publishes: the nest-relative path the reader
         *  was opened on, or `sealed:<hash>` for opened bytes. */
        val source: String

        /** A `Url` source, read through the pinned ranged reader. */
        class Streamed(override val source: String, val reader: FfiRangedNestReaderInterface) : Media

        /** A sealed video's opened bytes, on disk for `setDataSource(path)`. */
        class Opened(override val source: String, val file: File) : Media
    }

    /** The phase `video-thumbnail` publishes as its `state` (`drivers/base.py::get_attr`). */
    enum class Phase(val wire: String) { IDLE("idle"), LOADING("loading"), PLAYING("playing"), ERROR("error") }

    /** How a reader is opened — the FFI constructor in the app, a fake in the tests.
     *  Blocks on a network round trip: called off the main thread only. */
    fun interface ReaderOpener {
        fun open(nestUrl: String, path: String): FfiRangedNestReaderInterface
    }

    val ffiReaderOpener = ReaderOpener { nestUrl, path -> FfiRangedNestReader.open(nestUrl, path) }

    /** The block a post's `video-thumbnail` plays: its `Video` by hash, else a bridged
     *  post's `ProxiedVideo` by path (render-model.md § D6b, § D6c → *Proxied video*). */
    fun videoBlock(hash: String?, proxiedPath: String?): RenderBlock? = when {
        hash != null -> RenderBlock.Video(hash = hash, alt = "")
        proxiedPath != null -> RenderBlock.ProxiedVideo(path = proxiedPath, alt = "")
        else -> null
    }

    /**
     * Resolve what [block] plays. `null` when nothing does: the projection named it
     * unplayable, the reader could not open (an unreachable nest, a refused cert), or
     * a sealed item did not open — the element shows `error`, and a tap retries.
     *
     * [nestUrl] is the string the WS login minted on ([ApiClient.nodeUrl]), so the
     * reader's pinned client keys the same SPKI pin.
     */
    suspend fun resolve(
        manager: FfiFeedManager,
        api: ApiClient,
        block: RenderBlock,
        nestUrl: String,
        cacheDir: File,
        opener: ReaderOpener = ffiReaderOpener,
    ): Media? = when (val source = manager.playbackSource(block)) {
        is PlaybackSource.Url -> withContext(Dispatchers.IO) {
            runCatching { Media.Streamed(source.url, opener.open(nestUrl, source.url)) }.getOrNull()
        }
        is PlaybackSource.Sealed -> PostMediaOpen.fetchAndOpen(api, manager, source.hash)?.let { bytes ->
            withContext(Dispatchers.IO) {
                runCatching {
                    val file = File.createTempFile("inline-video-", ".bin", cacheDir)
                    file.writeBytes(bytes)
                    Media.Opened("sealed:${source.hash}", file)
                }.getOrNull()
            }
        }
        is PlaybackSource.Unplayable -> null
    }

    /** Free what [media] owns, once, with the player that read it: the reader's FFI
     *  handle, or the opened bytes' temp file. */
    fun release(media: Media) {
        when (media) {
            is Media.Streamed -> (media.reader as? AutoCloseable)?.close()
            is Media.Opened -> media.file.delete()
        }
    }
}

/**
 * The `MediaDataSource` `MediaPlayer` reads a `Url` source through — every byte one
 * `Range` GET over the shared pinned reader, the rules windows' `RangedNestCursor`
 * keeps: a known end clamps the read and answers end-of-stream without a request; a
 * reader answer narrower than asked is read on from where it stopped, so a "fill
 * this buffer" read is never mistaken for the end of the video; an unknown length
 * (a proxied upstream that named none) reports size `-1` and reads to the reader's
 * empty answer. `readAt` blocks — `MediaPlayer` calls it on its own thread.
 */
class RangedNestDataSource(private val reader: FfiRangedNestReaderInterface) : MediaDataSource() {
    private val length: Long? = reader.len()?.toLong()

    override fun getSize(): Long = length ?: -1L

    override fun readAt(position: Long, buffer: ByteArray, offset: Int, size: Int): Int {
        if (size <= 0) return 0
        val want = length?.let { minOf(size.toLong(), it - position) } ?: size.toLong()
        if (want <= 0) return -1
        var filled = 0
        while (filled < want) {
            val chunk = reader.readAt((position + filled).toULong(), (want - filled).toULong())
            if (chunk.isEmpty()) break
            val take = minOf(chunk.size.toLong(), want - filled).toInt()
            System.arraycopy(chunk, 0, buffer, offset + filled, take)
            filled += take
        }
        return if (filled == 0) -1 else filled
    }

    /** The reader is released with the player that owns it, never here: `MediaPlayer`
     *  closes its source before `release`, and the owner frees the FFI handle once. */
    override fun close() {}
}

/**
 * The `video-thumbnail` phase machine, pulled out of the composable so it is pinned
 * headlessly: `idle` until a tap (never autoplay); a tap → `loading`; a resolve that
 * names nothing → `error`; the player's prepared callback → `playing`, its error
 * callback → `error`. A tap while loading or playing is the player's own, never a
 * restart; a tap after an error retries. [generation] lets a late resolve or player
 * callback from a superseded attempt land nowhere.
 */
class VideoPlayerState {
    var phase: InlineVideoPlayback.Phase = InlineVideoPlayback.Phase.IDLE
        private set
    var source: String = ""
        private set
    var generation: Int = 0
        private set

    /** A tap. Returns the attempt's generation when it starts a resolve, `null` when the
     *  tap belongs to a player already loading or playing. */
    fun tap(): Int? {
        if (phase == InlineVideoPlayback.Phase.LOADING || phase == InlineVideoPlayback.Phase.PLAYING) return null
        phase = InlineVideoPlayback.Phase.LOADING
        source = ""
        return ++generation
    }

    /** The resolve's answer for attempt [gen]: `null` media is `error`. */
    fun resolved(gen: Int, media: InlineVideoPlayback.Media?): Boolean {
        if (gen != generation) return false
        if (media == null) phase = InlineVideoPlayback.Phase.ERROR else source = media.source
        return true
    }

    fun prepared(gen: Int) {
        if (gen == generation && phase == InlineVideoPlayback.Phase.LOADING) phase = InlineVideoPlayback.Phase.PLAYING
    }

    fun failed(gen: Int) {
        if (gen == generation) phase = InlineVideoPlayback.Phase.ERROR
    }
}
