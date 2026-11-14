using FaunaApp.Core.Helpers;
using FaunaApp.Helpers;
using Microsoft.UI.Dispatching;
using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.Storage.Streams;
using uniffi.fauna_core;

namespace FaunaApp.Controls;

/// <summary>
/// One tap's inline playback in a <c>video-thumbnail</c> slot — the resolve, WinUI's
/// <see cref="MediaPlayer"/>, the stream it reads and the phase walk
/// (<see cref="VideoPlaybackMachine"/>) — apart from the <see cref="VideoThumbnailView"/>
/// that shows it (<c>docs/goal/architecture/render-model.md</c> § D6c → <i>Inline
/// playback</i>). A list card's session is kept by its list
/// (<see cref="Feed.PostCardHost.Players"/>, an <see cref="InlinePlayerKeep{TPlayer}"/>), so
/// the card rebuilt for its post adopts it when the feed moves the row; the post-detail
/// dialog's is its view's alone.
///
/// <para>The session, never the view, talks to the cue capture: each progress reading
/// (<see cref="Feed.CueViewportObserver.NotePlayed"/>) and the player's engagement
/// (<see cref="Feed.CueViewportObserver.NotePlaying"/>; <c>engagement-cues.md</c> § Cue
/// vocabulary &amp; derivation, <i>A play is attention</i>) — so a card rebuilt mid-play
/// changes nothing the capture reads.</para>
///
/// <para><b>Threading:</b> UI thread only. The resolve awaits without
/// <c>ConfigureAwait(false)</c>, and the player's events (raised on its own threads) are
/// marshalled through the dispatcher the session was made on before they touch any state;
/// <see cref="Changed"/> is raised there too.</para>
/// </summary>
internal sealed class InlineVideoSession
{
    private readonly DispatcherQueue _dispatcher;
    private readonly VideoPlaybackMachine _machine = new();

    /// <summary>The post whose card started this session, for the cue capture; <c>null</c>
    /// for the post-detail dialog, whose player plays outside the list the capture
    /// samples.</summary>
    private readonly string? _postId;

    /// <summary>The stream the player reads, while it reads one — released with the player
    /// (a sealed video's bytes need no release).</summary>
    private PlaybackMedia.Streamed? _streamed;

    /// <summary>Bumped on every tap and stop, so a resolve that outlives its tap is
    /// dropped.</summary>
    private int _attempt;

    /// <summary>Whether this session last told the capture its player is engaged.</summary>
    private bool _engaged;

    internal InlineVideoSession(string? postId, DispatcherQueue dispatcher)
    {
        _postId = postId is { Length: > 0 } ? postId : null;
        _dispatcher = dispatcher;
    }

    /// <summary>Raised on the UI thread whenever <see cref="Player"/> or the phase changes,
    /// so the view showing the session repaints — and, when the player is going away, lets
    /// go of it BEFORE it is disposed (<see cref="ReleasePlayer"/>).</summary>
    internal event Action? Changed;

    /// <summary>The player, once the source resolved and it started.</summary>
    internal MediaPlayer? Player { get; private set; }

    /// <summary>The phase the slot paints and publishes.</summary>
    internal VideoPlaybackPhase Phase => _machine.Phase;

    /// <summary><c>state;position;source</c>, packed — what the slot's automation peer
    /// answers as HelpText, off the live player at read time.</summary>
    internal string AutomationAttributes()
        => _machine.AutomationAttributes(Player?.PlaybackSession.Position.TotalSeconds ?? 0);

    /// <summary>The tap: resolve what <paramref name="block"/> plays, then play it;
    /// <c>Play()</c> is called here and nowhere else. A tap while loading or playing belongs
    /// to the player's own controls (<see cref="VideoPlaybackMachine.Tap"/>); one on an error
    /// retries.</summary>
    internal async void Activate(RenderBlock block, uniffi.fauna_ffi.FfiPostBlobHome? home)
    {
        if (!_machine.Tap()) return;
        ReleasePlayer();
        var attempt = ++_attempt;
        // The tap engages the player, loading included (engagement-cues.md, A play is
        // attention): from here the post's media row is attended whatever its visible
        // fraction, until a pause, the end, a failure or the teardown releases it.
        NotePlaying(true);
        Changed?.Invoke();

        PlaybackMedia? media;
        try
        {
            media = await VideoPlaybackResolver.ResolveAsync(
                App.CurrentNest, Core.FeedManagerHost.Current, block,
                hosts: Core.PostBlobHostsHost.Current, home: home);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"InlineVideoSession: resolve {block} failed: {ex}");
            media = null;
        }
        if (attempt != _attempt)
        {
            // A resolve that outlived its tap still opened a reader: close it here, since
            // no player will.
            (media as PlaybackMedia.Streamed)?.Release();
            return;
        }

        _machine.Resolved(media);
        if (_machine.Media is not { } resolved)
        {
            NotePlaying(false);
            Changed?.Invoke();
            return;
        }
        await StartPlayerAsync(resolved, attempt);
    }

    /// <summary>Back to the idle glyph for good, releasing the player and the engagement.
    /// Idempotent.</summary>
    internal void Stop()
    {
        _attempt++;
        NotePlaying(false);
        ReleasePlayer();
        _machine.Reset();
        Changed?.Invoke();
    }

    private async Task StartPlayerAsync(PlaybackMedia media, int attempt)
    {
        MediaSource source;
        switch (media)
        {
            case PlaybackMedia.Streamed streamed:
                // The player reads the nest's bytes through the pinned reader the resolver
                // opened — WinUI's media stack never dials the nest (security.md § Transport
                // trust → The native-player leg). The session owns the reader from here and
                // releases it with the player.
                source = MediaSource.CreateFromStream(
                    new RangedNestStream(new RangedNestCursor(streamed.Reader)), streamed.ContentType);
                _streamed = streamed;
                break;
            case PlaybackMedia.Opened opened:
                // Held bytes — a sealed video's plaintext, or a plain video fetched whole
                // from the post's own home — play from memory: they never touch disk, so
                // no temp file outlives the session.
                source = MediaSource.CreateFromStream(await InMemoryAsync(opened.Bytes), opened.ContentType);
                if (attempt != _attempt) return;
                break;
            default:
                return;
        }

        var player = new MediaPlayer { AutoPlay = false, Source = source };
        // Whether this player has reached Playing yet: a Paused or None reported while it
        // opens, before the first play, is not the user pausing, so it keeps the tap's
        // engagement (apple drops its initial `.paused` the same way). Touched only on the
        // UI thread, inside the enqueued callbacks.
        var hasPlayed = false;
        player.PlaybackSession.PlaybackStateChanged += (session, _) =>
        {
            // Playing engages the cue capture's attention; a later pause or an idle player
            // releases it. Opening and buffering change nothing.
            var state = session.PlaybackState;
            if (state is not (MediaPlaybackState.Playing or MediaPlaybackState.Paused or MediaPlaybackState.None)) return;
            _dispatcher.TryEnqueue(() =>
            {
                if (!ReferenceEquals(Player, player)) return;
                if (state == MediaPlaybackState.Playing)
                {
                    hasPlayed = true;
                    _machine.PlayerStartedPlaying();
                    NotePlaying(true);
                    Changed?.Invoke();
                }
                else if (hasPlayed)
                {
                    NotePlaying(false);
                }
            });
        };
        // Every progress report, the end included (an ended player reads as played to
        // its end), is noted for the cue capture — web's `notePlayed` on `timeupdate`
        // and `ended`.
        player.PlaybackSession.PositionChanged += (session, _) =>
        {
            var position = session.Position.TotalSeconds;
            var duration = session.NaturalDuration.TotalSeconds;
            _dispatcher.TryEnqueue(() =>
            {
                if (ReferenceEquals(Player, player)) NotePlayed(position, duration);
            });
        };
        player.MediaEnded += (sender, _) =>
        {
            var duration = sender.PlaybackSession.NaturalDuration.TotalSeconds;
            _dispatcher.TryEnqueue(() =>
            {
                if (!ReferenceEquals(Player, player)) return;
                NotePlayed(duration, duration);
                NotePlaying(false);
            });
        };
        player.MediaFailed += (_, args) =>
        {
            System.Diagnostics.Debug.WriteLine(
                $"InlineVideoSession: player failed ({args.Error}): {args.ErrorMessage}");
            _dispatcher.TryEnqueue(() =>
            {
                if (!ReferenceEquals(Player, player)) return;
                _machine.PlayerFailed();
                NotePlaying(false);
                Changed?.Invoke();
            });
        };

        Player = player;
        // The view showing the session puts the player in its slot before it plays.
        Changed?.Invoke();
        player.Play();
    }

    /// <summary>Let go of the player and its stream. The view showing the player is told
    /// first (<see cref="Changed"/>, with <see cref="Player"/> already <c>null</c>), so its
    /// <c>MediaPlayerElement</c> lets go of the player and leaves the slot BEFORE the player
    /// is disposed: an element still holding a disposed player faults inside XAML's own
    /// callbacks, and that stowed exception kills the process (0xC000027B, faulting in
    /// CoreMessagingXP.dll) — measured whenever a playing slot unloaded: leaving the feed
    /// page, the window closing, the list rebuilding its rows.</summary>
    private void ReleasePlayer()
    {
        if (Player is { } player)
        {
            Player = null;
            Changed?.Invoke();
            player.Pause();
            player.Source = null;
            player.Dispose();
        }
        // After the player: the reader outlives every read the player may still have in
        // flight (the binding frees it only once those return).
        _streamed?.Release();
        _streamed = null;
    }

    private void NotePlayed(double position, double duration)
    {
        if (_postId is { } postId)
            Feed.CueViewportObserver.NotePlayed(postId, position, duration);
        else
            Core.Logs.E2eTrace.Write($"[cue] note dropped: no PostId (position={position:0.###})");
    }

    /// <summary>Hand the player's engagement to the cue capture
    /// (<see cref="Feed.CueViewportObserver.NotePlaying"/>), a release only after an
    /// engagement. The post-detail dialog's session has no post: it plays outside the list
    /// the capture samples.</summary>
    private void NotePlaying(bool playing)
    {
        if (!playing && !_engaged) return;
        _engaged = playing;
        if (_postId is { } postId)
            Feed.CueViewportObserver.NotePlaying(postId, playing);
    }

    private static async Task<IRandomAccessStream> InMemoryAsync(byte[] bytes)
    {
        var stream = new InMemoryRandomAccessStream();
        using (var writer = new DataWriter(stream))
        {
            writer.WriteBytes(bytes);
            await writer.StoreAsync();
            writer.DetachStream();
        }
        stream.Seek(0);
        return stream;
    }
}
