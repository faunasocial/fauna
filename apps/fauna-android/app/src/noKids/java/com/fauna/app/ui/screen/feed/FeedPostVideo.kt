package com.fauna.app.ui.screen.feed

import android.media.MediaPlayer
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PlayCircleOutline
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import com.fauna.app.core.feed.InlineVideoPlayback
import com.fauna.app.core.feed.PlaybackPeaks
import com.fauna.app.core.feed.RangedNestDataSource
import com.fauna.app.core.feed.VideoPlayerState
import social.fauna.generated.Ids
import com.fauna.app.ui.components.documentMediaVideoHash
import com.fauna.app.ui.components.documentProxiedPostVideoPath
import com.fauna.app.ui.viewmodel.FeedVM
import com.fauna.ffi.cueSampleIntervalMs
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.fauna_core.RenderDocument

/**
 * The feed post video attachment (ui.yaml `video-thumbnail`) — the inline player host
 * (render-model.md § D6c → *Inline playback*). Painted from the folded `Video` block of
 * the post `document` (§ D6b; [FeedPostImage]'s `LaunchedEffect` already fires the
 * fire-once `resolve_media` fold), or a bridged post's `ProxiedVideo` (§ D6c → *Proxied
 * video*): the play glyph + the hash (or nest-relative path) until a tap — no poster
 * frame exists to paint, and nothing is fetched before the tap (never autoplay). A tap
 * asks the shared `playbackSource` projection what plays ([InlineVideoPlayback]) and
 * swaps the glyph for the framework `MediaPlayer` on a `SurfaceView` in the same element.
 * The element publishes its phase ([VideoPlayerState]) as `stateDescription`, and the
 * player's progress feeds [postId]'s engagement-cue row ([PlaybackPeaks]). Renders
 * nothing when the document has no video block.
 */
@Composable
fun FeedPostVideo(document: RenderDocument, postId: String, vm: FeedVM) {
    val hash = documentMediaVideoHash(document)
    val proxiedPath = if (hash == null) documentProxiedPostVideoPath(document) else null
    val block = InlineVideoPlayback.videoBlock(hash, proxiedPath) ?: return
    val label = hash ?: proxiedPath ?: return
    val cacheDir = LocalContext.current.cacheDir
    val scope = rememberCoroutineScope()
    val machine = remember(label) { VideoPlayerState() }
    var phase by remember(label) { mutableStateOf(machine.phase) }
    var media by remember(label) { mutableStateOf<InlineVideoPlayback.Media?>(null) }
    var generation by remember(label) { mutableIntStateOf(0) }

    Box(
        modifier = Modifier
            .testTag(Ids.VIDEO_THUMBNAIL)
            .semantics { stateDescription = phase.wire }
            .clickable {
                val gen = machine.tap() ?: return@clickable
                generation = gen
                media = null
                phase = machine.phase
                scope.launch {
                    val resolved = vm.resolvePlayback(postId, block, cacheDir)
                    if (machine.resolved(gen, resolved)) {
                        media = resolved
                    } else {
                        resolved?.let(InlineVideoPlayback::release)
                    }
                    phase = machine.phase
                }
            },
    ) {
        val playing = media
        if (playing != null) {
            InlineVideoSurface(
                media = playing,
                postId = postId,
                onPrepared = {
                    machine.prepared(generation)
                    phase = machine.phase
                },
                onError = {
                    machine.failed(generation)
                    phase = machine.phase
                    media = null
                },
            )
        } else {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Default.PlayCircleOutline, contentDescription = null, modifier = Modifier.size(20.dp))
                Spacer(Modifier.width(4.dp))
                Text(label, style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}

/**
 * The framework `MediaPlayer` on a `SurfaceView`, reading [media] — a `Url` source
 * through [RangedNestDataSource], a sealed one from its opened temp file. The player is
 * prepared once the surface exists, started on prepared, and released — with the
 * reader or temp file it owns — when this leaves composition. While it plays, its
 * position is noted for [postId] on the shared sampling tick, and its completion as the
 * whole play.
 */
@Composable
private fun InlineVideoSurface(
    media: InlineVideoPlayback.Media,
    postId: String,
    onPrepared: () -> Unit,
    onError: () -> Unit,
) {
    val player = remember(media) { MediaPlayer() }
    // `surfaceCreated` fires again after a scroll destroys the surface; the player is
    // prepared once, and a re-created surface only gets the display back.
    val prepared = remember(media) { booleanArrayOf(false) }
    DisposableEffect(media) {
        val started = runCatching {
            when (media) {
                is InlineVideoPlayback.Media.Streamed -> player.setDataSource(RangedNestDataSource(media.reader))
                is InlineVideoPlayback.Media.Opened -> player.setDataSource(media.file.path)
            }
            player.setOnPreparedListener {
                it.start()
                onPrepared()
            }
            player.setOnCompletionListener {
                runCatching { PlaybackPeaks.note(postId, it.duration, it.duration) }
            }
            player.setOnErrorListener { _, _, _ ->
                onError()
                true
            }
        }.isSuccess
        if (!started) onError()
        onDispose {
            player.release()
            InlineVideoPlayback.release(media)
        }
    }
    LaunchedEffect(media) {
        val intervalMs = cueSampleIntervalMs().toLong()
        while (true) {
            delay(intervalMs)
            // A player not yet prepared, or already released, throws: no reading.
            runCatching {
                if (player.isPlaying) PlaybackPeaks.note(postId, player.currentPosition, player.duration)
            }
        }
    }
    AndroidView(
        modifier = Modifier.size(width = 320.dp, height = 180.dp),
        factory = { context ->
            SurfaceView(context).apply {
                holder.addCallback(object : SurfaceHolder.Callback {
                    override fun surfaceCreated(holder: SurfaceHolder) {
                        runCatching {
                            player.setDisplay(holder)
                            if (!prepared[0]) {
                                prepared[0] = true
                                player.prepareAsync()
                            }
                        }.onFailure { onError() }
                    }

                    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

                    override fun surfaceDestroyed(holder: SurfaceHolder) {
                        runCatching { player.setDisplay(null) }
                    }
                })
            }
        },
    )
}
