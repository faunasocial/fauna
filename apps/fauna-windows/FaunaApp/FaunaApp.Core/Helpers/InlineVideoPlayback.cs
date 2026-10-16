using System.Globalization;
using FaunaApp.Core.Services;
using uniffi.fauna_core;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

// Inline video playback in the `video-thumbnail` slot — the windows glue around the
// shared decision of what plays (`docs/goal/architecture/render-model.md` § D6c →
// *Inline playback*). `FeedManager::playback_source` says WHAT plays; everything here is
// the player's own transient state, which the ruling keeps out of the model: the phase
// the element publishes, and the turn of a `PlaybackSource` into what WinUI's
// `MediaPlayerElement` is handed. The player itself is `FaunaApp.Controls
// .VideoThumbnailView` — WinUI, which the unit-test project cannot reference, so every
// decision lives here, pinned by `InlineVideoPlaybackTests`. The twin of apple's
// FaunaKit `InlineVideoPlayback.swift`.

/// <summary>The <c>state</c> the <c>video-thumbnail</c> element publishes — the four
/// strings <c>tests/e2e-unified/drivers/base.py::get_attr</c> documents for every app
/// (<see cref="VideoPlaybackMachine.Wire"/>).</summary>
public enum VideoPlaybackPhase
{
    /// <summary>The play glyph; nothing was asked for. A video never leaves this by
    /// itself.</summary>
    Idle,
    /// <summary>Tapped: the source is resolving, or the player is buffering.</summary>
    Loading,
    /// <summary>The native player reported it is playing.</summary>
    Playing,
    /// <summary>Nothing playable, or the player's own failure.</summary>
    Error,
}

/// <summary>What the native player is handed, once the shared decision resolved.</summary>
public abstract record PlaybackMedia
{
    private PlaybackMedia() { }

    /// <summary>A nest-relative path the player reads through <paramref name="Reader"/> —
    /// the shared pinned ranged reader, opened on it — never a URL WinUI's media stack
    /// would dial against the OS trust store alone (security.md § Transport trust →
    /// <i>The native-player leg</i>). A public <c>Video</c> off the unauthenticated blob
    /// route, or a bridged <c>ProxiedVideo</c>'s proxied path whose playback ticket
    /// (<c>exp</c> + <c>sig</c>) rides its query: neither needs a bearer, so the reader
    /// carries none. <see cref="RangedNestCursor"/> reads it; whoever starts the player
    /// owns the reader and <see cref="Release"/>s it with the player.</summary>
    internal sealed record Streamed(string Path, string ContentType, IFfiRangedNestReader Reader)
        : PlaybackMedia
    {
        /// <summary>Close the reader. Safe while a read is in flight — the binding frees the
        /// Rust object only once that read returns.</summary>
        internal void Release() => (Reader as IDisposable)?.Dispose();
    }

    /// <summary>A sealed <c>Video</c>'s plaintext, opened by the manager: no URL can serve
    /// it, so it plays from memory — whole blob, no <c>Range</c> (the ruling's sealed
    /// arm).</summary>
    public sealed record Opened(string Hash, byte[] Bytes, string ContentType) : PlaybackMedia;

    /// <summary>The <c>source</c> attribute: the nest-relative path a stream reads, or
    /// <c>sealed:&lt;hash&gt;</c> for opened bytes, which have none.</summary>
    public string Source => this switch
    {
        Streamed streamed => streamed.Path,
        Opened opened => "sealed:" + opened.Hash,
        _ => "",
    };
}

/// <summary>
/// The phase walk, as a pure value so the rule can be tested without a player — the twin
/// of apple's <c>VideoPlaybackMachine</c> and web's <c>VideoThumbnail.svelte</c>
/// <c>playState</c>.
/// </summary>
public sealed class VideoPlaybackMachine
{
    public VideoPlaybackPhase Phase { get; private set; } = VideoPlaybackPhase.Idle;

    /// <summary>What the player was handed, once the source resolved.</summary>
    public PlaybackMedia? Media { get; private set; }

    /// <summary>The tap. <c>true</c> when it starts a resolve — only from <c>idle</c>, or
    /// from <c>error</c> as the retry (which drops the failed source); a tap while loading
    /// or playing belongs to the native player's own controls.</summary>
    public bool Tap()
    {
        if (Phase is not (VideoPlaybackPhase.Idle or VideoPlaybackPhase.Error)) return false;
        Phase = VideoPlaybackPhase.Loading;
        Media = null;
        return true;
    }

    /// <summary>The source resolved: <c>null</c> is nothing playable. Media is not
    /// playback yet — the phase stays <c>loading</c> until the player says
    /// otherwise.</summary>
    public void Resolved(PlaybackMedia? media)
    {
        if (Phase != VideoPlaybackPhase.Loading || Media is not null) return;
        if (media is null)
        {
            Phase = VideoPlaybackPhase.Error;
            return;
        }
        Media = media;
    }

    /// <summary>The player reported <c>Playing</c>.</summary>
    public void PlayerStartedPlaying()
    {
        if (Media is null || Phase != VideoPlaybackPhase.Loading) return;
        Phase = VideoPlaybackPhase.Playing;
    }

    /// <summary>The player's media failed; its own error surface is the error UI.</summary>
    public void PlayerFailed()
    {
        if (Media is null) return;
        Phase = VideoPlaybackPhase.Error;
    }

    /// <summary>Back to the play glyph (the slot was recycled, or left the screen).</summary>
    public void Reset()
    {
        Phase = VideoPlaybackPhase.Idle;
        Media = null;
    }

    /// <summary>The wire spelling of a phase.</summary>
    public static string Wire(VideoPlaybackPhase phase) => phase switch
    {
        VideoPlaybackPhase.Idle => "idle",
        VideoPlaybackPhase.Loading => "loading",
        VideoPlaybackPhase.Playing => "playing",
        _ => "error",
    };

    /// <summary>
    /// <c>state</c>, <c>position</c> and <c>source</c>, packed into the one HelpText UIA
    /// gives the element (<see cref="AutomationAttributePack"/>). The position is the
    /// player's own, read by the caller at the moment UIA asks; it is formatted
    /// culture-invariantly, because it is a wire value the drivers parse, never a display
    /// one.
    /// </summary>
    public string AutomationAttributes(double positionSeconds) => AutomationAttributePack.Pack(
        ("state", Wire(Phase)),
        ("position", positionSeconds.ToString("0.000", CultureInfo.InvariantCulture)),
        ("source", Media?.Source ?? ""));
}

/// <summary>
/// Several automation attributes in the one HelpText UIA gives an element: <c>key=value</c>
/// pairs joined by <c>;</c>, with <c>%</c> written <c>%25</c> and <c>;</c> written
/// <c>%3B</c> inside a value. The FlaUI bridge reads one key back
/// (<c>tests/e2e-unified/flaui-bridge/PackedAttr.cs</c> — mirrored there, since the bridge
/// links no app code), so a test's <c>get_attr(id, key)</c> reads the same on windows as on
/// every other app. Keys are lowercase kebab; a pack holds two or more pairs (one value
/// needs no packing, and the bridge reads a single pair as a bare value).
/// </summary>
public static class AutomationAttributePack
{
    public static string Pack(params (string Key, string Value)[] pairs)
        => string.Join(";", pairs.Select(p => $"{p.Key}={Escape(p.Value)}"));

    private static string Escape(string value) => value.Replace("%", "%25").Replace(";", "%3B");
}

/// <summary>
/// The turn of the shared <c>FeedManager::playback_source</c> decision into
/// <see cref="PlaybackMedia"/>: the windows reading of what web's <c>playbackUrl</c> and
/// apple's <c>FeedVM.videoPlaybackURL</c> do.
/// </summary>
internal static class VideoPlaybackResolver
{
    /// <summary>
    /// Ask the manager what the tapped <paramref name="block"/> — the slot's folded
    /// <c>Video</c> or <c>ProxiedVideo</c> — plays from, and make it something the player
    /// reads: a nest-relative path opens the shared pinned ranged reader on this nest
    /// (<see cref="INestHttpClient.NestUrl"/> — the blob route is unauthenticated by design,
    /// and a proxied path arrives already carrying the playback ticket
    /// <c>playback_source</c> minted, so the reader needs no bearer either way); a sealed
    /// item goes through the same fetch + <c>OpenMediaBytes</c> a sealed image does
    /// (<see cref="PostMediaOpen"/>'s rule: <c>null</c> from the open means it did not open,
    /// and ciphertext never reaches a decoder). <c>null</c> is nothing playable —
    /// <c>Unplayable</c> (a refused ticket included), no session yet, a path that is not
    /// this nest's, or a reader or fetch that failed — which the element shows as
    /// <c>error</c>.
    /// </summary>
    /// <param name="nest">The current nest client (resolved per call by the caller — see
    /// <c>BlobImageLoader</c>'s note on disposed clients).</param>
    /// <param name="manager">The session's feed manager, or <c>null</c> before one is
    /// built.</param>
    /// <param name="block">What the slot holds (<see cref="DocumentRenderer.MediaVideoBlock"/>).
    /// The manager is asked about the block itself because a <c>ProxiedVideo</c>'s ticket
    /// is minted inside the decision — no app builds that URL by hand (render-model.md
    /// § D6c → <i>Inline playback</i>, answer 4).</param>
    /// <param name="openReader">Opens the ranged reader on (nest URL, path) — the UniFFI
    /// <c>FfiRangedNestReader.Open</c>, or a fake in the unit tests.</param>
    internal static async Task<PlaybackMedia?> ResolveAsync(
        INestHttpClient? nest, IFfiFeedManager? manager, RenderBlock block, CancellationToken ct = default,
        Func<string, string, IFfiRangedNestReader>? openReader = null)
    {
        if (nest is null || manager is null) return null;
        try
        {
            switch (await manager.PlaybackSource(block))
            {
                // Both `Url` arms — a public `Video`'s blob route and a `ProxiedVideo`'s
                // ticketed proxy path — take this one branch: one reader, one byte source.
                case PlaybackSource.Url url:
                    return await OpenStreamedAsync(
                        nest.NestUrl, url.url, openReader ?? FfiRangedNestReader.Open, ct);
                case PlaybackSource.Sealed sealedItem:
                    var fetched = await nest.GetBlobAsync(sealedItem.hash, ct);
                    var opened = manager.OpenMediaBytes(sealedItem.hash, fetched);
                    return opened is null
                        ? null
                        : new PlaybackMedia.Opened(sealedItem.hash, opened, ContentTypeOf(opened));
                default:
                    return null;
            }
        }
        catch (Exception ex)
        {
            // Nothing playable, traced — never a crash out of a tap.
            System.Diagnostics.Debug.WriteLine($"VideoPlaybackResolver: {block} did not resolve: {ex}");
            return null;
        }
    }

    /// <summary>
    /// Open the reader on <paramref name="path"/> and learn what it plays as, off the UI
    /// thread: the open is a blocking ranged probe, and the content-type sniff may be one
    /// more read. A path that is not nest-relative is refused before anything is dialed —
    /// the reader is pinned to this nest, and a path naming another origin is not this
    /// nest's to play (<see cref="NestContentPath"/>).
    /// </summary>
    private static async Task<PlaybackMedia?> OpenStreamedAsync(
        string nestUrl, string path, Func<string, string, IFfiRangedNestReader> openReader,
        CancellationToken ct)
    {
        if (!NestContentPath.IsNestRelative(path)) return null;
        return await Task.Run<PlaybackMedia?>(() =>
        {
            var reader = openReader(nestUrl, path);
            try
            {
                return new PlaybackMedia.Streamed(path, ContentTypeOf(reader), reader);
            }
            catch
            {
                (reader as IDisposable)?.Dispose();
                throw;
            }
        }, ct);
    }

    /// <summary>
    /// The content type a nest stream plays as: the nest's own, when it names a video or an
    /// audio type (a proxied fediverse <c>video/webm</c> is not an MP4); otherwise — the blob
    /// route serves an upload whose stored type was not one, as
    /// <c>application/octet-stream</c> — the brand of its first bytes
    /// (<see cref="ContentTypeOf(byte[])"/>), read through the reader.
    /// </summary>
    internal static string ContentTypeOf(IFfiRangedNestReader reader)
    {
        var declared = reader.ContentType()?.Split(';')[0].Trim();
        return declared is not null
               && (declared.StartsWith("video/", StringComparison.OrdinalIgnoreCase)
                   || declared.StartsWith("audio/", StringComparison.OrdinalIgnoreCase))
            ? declared
            : ContentTypeOf(reader.ReadAt(0, 12));
    }

    /// <summary>
    /// The content type an in-memory stream plays as. The <c>Video</c> block deliberately
    /// carries no mime (the image-vs-video branch is made once, in the shared fold), so
    /// read the ISO-BMFF brand: QuickTime's <c>qt  </c> is <c>video/quicktime</c>,
    /// everything else <c>video/mp4</c> — apple's <c>SealedPlaybackFile.fileExtension</c>
    /// rule.
    /// </summary>
    internal static string ContentTypeOf(byte[] bytes)
        => bytes.Length >= 12
           && bytes[4] == (byte)'f' && bytes[5] == (byte)'t' && bytes[6] == (byte)'y' && bytes[7] == (byte)'p'
           && bytes[8] == (byte)'q' && bytes[9] == (byte)'t' && bytes[10] == (byte)' ' && bytes[11] == (byte)' '
            ? "video/quicktime"
            : "video/mp4";
}
