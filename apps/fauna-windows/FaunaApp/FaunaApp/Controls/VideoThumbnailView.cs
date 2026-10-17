using FaunaApp.Core.Helpers;
using FaunaApp.Helpers;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.Storage.Streams;
using uniffi.fauna_core;

namespace FaunaApp.Controls;

/// <summary>
/// The feed's <c>video-thumbnail</c> element — and, since inline playback, the player host
/// (<c>docs/goal/architecture/render-model.md</c> § D6c → <i>Inline playback</i>). Idle it
/// is the play glyph + the hash (or a bridged video's path) as text (no poster frame exists to paint —
/// <c>MediaItem.thumbnail</c> is <c>None</c> from every writer); a tap asks the shared
/// <c>FeedManager::playback_source</c> what plays (<see cref="VideoPlaybackResolver"/>) and
/// swaps the glyph for WinUI's own <see cref="MediaPlayerElement"/> in the same slot — a
/// platform framework, no third-party player — fed a byte source, never a URL: a nest
/// stream through <see cref="RangedNestStream"/> over the shared pinned reader, a sealed
/// video from memory. Never autoplay: the tap is the one
/// <c>Play()</c>. The player's own transport controls and error surface are the UI; no new
/// element. The windows twin of apple's <c>VideoThumbnailView</c> and web's
/// <c>VideoThumbnail.svelte</c>.
///
/// <para><b>One control, both call sites:</b> the list card's DataTemplate
/// (<c>FeedPage.xaml</c>) and the imperative post-detail dialog
/// (<c>FeedPage.BuildPostDetailPanel</c>), both handing it the one block the slot plays
/// (<see cref="Block"/>, off <c>DocumentRenderer.MediaVideoBlock</c>): a blob <c>Video</c>,
/// painted and named by its hash, or a bridged <c>ProxiedVideo</c>, painted and named by
/// its nest-relative path and never byte-loaded before the tap. The tap asks the shared
/// decision about that block, so a <c>ProxiedVideo</c> plays off the ticketed path
/// <c>playback_source</c> answers for it — this view never builds a URL.</para>
///
/// <para><b>What the drivers read</b> (<c>drivers/base.py::get_attr</c>): <c>state</c>
/// (<c>idle</c> / <c>loading</c> / <c>playing</c> / <c>error</c>), <c>position</c> (the
/// player's seconds) and <c>source</c> (what it was handed). UIA carries one
/// application-supplied string per element, so the automation peer packs all three into
/// its HelpText (<see cref="AutomationAttributePack"/>) and the bridge answers each key on
/// its own. The peer computes the string AT READ TIME off the live player, so the position
/// a test polls is the player's own and no timer republishes it. The phase walk itself is
/// the pure <see cref="VideoPlaybackMachine"/>, pinned by <c>InlineVideoPlaybackTests</c>;
/// this control is only the glue that feeds it player events.</para>
///
/// <para><b>Threading:</b> every machine transition runs on the UI thread — the resolve
/// awaits without <c>ConfigureAwait(false)</c>, and the player's events (raised on its own
/// threads) are marshalled through <see cref="FrameworkElement.DispatcherQueue"/> — which
/// is also where UIA calls the peer, so the published phase is never read mid-write.</para>
/// </summary>
public sealed class VideoThumbnailView : UserControl
{
    private readonly VideoPlaybackMachine _machine = new();
    private readonly Grid _slot = new();
    private readonly StackPanel _row = new() { Orientation = Orientation.Horizontal, Spacing = 4 };
    private readonly TextBlock _glyph = new()
    {
        Text = "",
        FontFamily = new FontFamily("Segoe MDL2 Assets"),
        FontSize = 14,
        VerticalAlignment = VerticalAlignment.Center,
    };
    private readonly ProgressRing _spinner = new()
    {
        Width = 14,
        Height = 14,
        IsActive = true,
        VerticalAlignment = VerticalAlignment.Center,
    };
    private readonly TextBlock _label = new()
    {
        FontSize = 12,
        Opacity = 0.7,
        VerticalAlignment = VerticalAlignment.Center,
    };
    private readonly Button _button = new()
    {
        Padding = new Thickness(0),
        BorderThickness = new Thickness(0),
        Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent),
    };

    private RenderBlock? _block;
    private MediaPlayer? _player;
    /// <summary>The element showing <see cref="_player"/>, while one does — detached from
    /// the player before the player is disposed (<see cref="TearDownPlayer"/>).</summary>
    private MediaPlayerElement? _element;
    /// <summary>The stream the player reads, while it reads one — kept so its reader is
    /// released with the player (a sealed video's bytes need no release).</summary>
    private PlaybackMedia.Streamed? _streamed;
    /// <summary>Bumped on every tap and stop, so a resolve that outlives its tap — or a
    /// row recycled onto another post while it ran — is dropped.</summary>
    private int _attempt;

    public VideoThumbnailView()
    {
        _row.Children.Add(_glyph);
        _row.Children.Add(_spinner);
        _row.Children.Add(_label);
        _button.Click += (_, _) => Activate();
        Content = _slot;
        // A row scrolled away or a dialog closed releases its player: nothing plays
        // where the reader cannot see it, and the decoder's memory goes with it.
        Unloaded += (_, _) => Stop();
        Paint();
    }

    /// <summary>The block this slot plays — a <c>RenderBlock.Video</c> or
    /// <c>RenderBlock.ProxiedVideo</c>, or <c>null</c> for nothing to play. Typed
    /// <c>object</c> because the UniFFI-generated <c>RenderBlock</c> is <c>internal</c>, which
    /// the XAML compiler cannot bind; any other value is nothing to play. Setting it paints
    /// the text beside the glyph — the hash, or the nest-relative path — which is also the
    /// element's accessible name (a DataTemplate element with no name is pruned from UIA, so
    /// FlaUI would count 0). A new block — a recycled row now showing another post —
    /// returns the slot to its idle glyph.</summary>
    public object? Block
    {
        get => _block;
        set
        {
            var next = value as RenderBlock;
            if (Equals(next, _block)) return;
            _block = next;
            var text = next switch
            {
                RenderBlock.Video video => video.hash,
                RenderBlock.ProxiedVideo proxied => proxied.path,
                _ => "",
            };
            _label.Text = text;
            AutomationProperties.SetName(this, text);
            Stop();
        }
    }

    /// <summary><c>state;position;source</c>, packed — what the automation peer answers as
    /// HelpText. Read on the UI thread (UIA's provider calls are marshalled there).</summary>
    internal string AutomationAttributes()
        => _machine.AutomationAttributes(_player?.PlaybackSession.Position.TotalSeconds ?? 0);

    /// <summary>The tap — the button, or UIA's Invoke. Resolves what plays, then plays it;
    /// <c>Play()</c> is called here and nowhere else.</summary>
    internal async void Activate()
    {
        if (_block is not { } block || !_machine.Tap()) return;
        TearDownPlayer();
        var attempt = ++_attempt;
        Paint();

        PlaybackMedia? media;
        try
        {
            media = await VideoPlaybackResolver.ResolveAsync(
                App.CurrentNest, Core.FeedManagerHost.Current, block);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"VideoThumbnailView: resolve {block} failed: {ex}");
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
            Paint();
            return;
        }
        await StartPlayerAsync(resolved, attempt);
    }

    private async Task StartPlayerAsync(PlaybackMedia media, int attempt)
    {
        MediaSource source;
        switch (media)
        {
            case PlaybackMedia.Streamed streamed:
                // The player reads the nest's bytes through the pinned reader the resolver
                // opened — WinUI's media stack never dials the nest (security.md § Transport
                // trust → The native-player leg). The view owns the reader from here and
                // releases it with the player.
                source = MediaSource.CreateFromStream(
                    new RangedNestStream(new RangedNestCursor(streamed.Reader)), streamed.ContentType);
                _streamed = streamed;
                break;
            case PlaybackMedia.Opened opened:
                // The plaintext of a sealed video plays from memory: it never touches
                // disk, so no temp file outlives the session.
                source = MediaSource.CreateFromStream(await InMemoryAsync(opened.Bytes), opened.ContentType);
                if (attempt != _attempt) return;
                break;
            default:
                return;
        }

        var player = new MediaPlayer { AutoPlay = false, Source = source };
        player.PlaybackSession.PlaybackStateChanged += (session, _) =>
        {
            if (session.PlaybackState != MediaPlaybackState.Playing) return;
            DispatcherQueue.TryEnqueue(() =>
            {
                if (ReferenceEquals(_player, player)) _machine.PlayerStartedPlaying();
            });
        };
        player.MediaFailed += (_, args) =>
        {
            System.Diagnostics.Debug.WriteLine(
                $"VideoThumbnailView: player failed ({args.Error}): {args.ErrorMessage}");
            DispatcherQueue.TryEnqueue(() =>
            {
                if (ReferenceEquals(_player, player)) _machine.PlayerFailed();
            });
        };

        var element = new MediaPlayerElement
        {
            AreTransportControlsEnabled = true,
            AutoPlay = false,
            Height = 200,
            HorizontalAlignment = HorizontalAlignment.Left,
            Stretch = Stretch.Uniform,
        };
        element.SetMediaPlayer(player);
        _player = player;
        _element = element;
        _slot.Children.Clear();
        _slot.Children.Add(element);
        player.Play();
    }

    /// <summary>Back to the idle glyph, releasing any player.</summary>
    private void Stop()
    {
        _attempt++;
        TearDownPlayer();
        _machine.Reset();
        Paint();
    }

    private void TearDownPlayer()
    {
        // The element lets go of the player, and leaves the slot, BEFORE the player is
        // disposed. A MediaPlayerElement still holding a disposed MediaPlayer faults inside
        // XAML's own callbacks, and that stowed exception kills the process (0xC000027B,
        // faulting in CoreMessagingXP.dll). Measured whenever a playing slot unloads:
        // leaving the feed page, the window closing, the list rebuilding its rows.
        if (_element is { } element)
        {
            _element = null;
            element.SetMediaPlayer(null);
            _slot.Children.Remove(element);
        }
        if (_player is { } player)
        {
            _player = null;
            player.Pause();
            player.Source = null;
            player.Dispose();
        }
        // After the player: the reader outlives every read the player may still have in
        // flight (the binding frees it only once those return).
        _streamed?.Release();
        _streamed = null;
    }

    /// <summary>The slot without a player: the glyph (a spinner while resolving) + the
    /// text, inside a button when there is something to play, bare when there is not.
    /// An idle slot with nothing to play is disabled, so a tap on it is the named refusal
    /// rather than a silent no-op.</summary>
    private void Paint()
    {
        if (_player is not null) return;
        var loading = _machine.Phase == VideoPlaybackPhase.Loading;
        _glyph.Visibility = loading ? Visibility.Collapsed : Visibility.Visible;
        _spinner.Visibility = loading ? Visibility.Visible : Visibility.Collapsed;

        _slot.Children.Clear();
        _button.Content = null;
        var playable = _block is not null;
        IsEnabled = playable;
        if (playable)
        {
            _button.Content = _row;
            _slot.Children.Add(_button);
        }
        else
        {
            _slot.Children.Add(_row);
        }
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

    protected override AutomationPeer OnCreateAutomationPeer()
        => new VideoThumbnailViewAutomationPeer(this);
}

/// <summary>
/// The <c>video-thumbnail</c> element as UIA sees it: invokable (the tap — the bridge's
/// click prefers Invoke, which needs no foreground window) and answering its HelpText from
/// the live player (<see cref="VideoThumbnailView.AutomationAttributes"/>) rather than a
/// stored string. Prior art: <c>DocumentBodyViewAutomationPeer</c>.
/// </summary>
internal sealed class VideoThumbnailViewAutomationPeer
    : FrameworkElementAutomationPeer, IInvokeProvider
{
    private readonly VideoThumbnailView _owner;

    public VideoThumbnailViewAutomationPeer(VideoThumbnailView owner) : base(owner)
        => _owner = owner;

    protected override object GetPatternCore(PatternInterface patternInterface)
        => patternInterface == PatternInterface.Invoke
            ? this
            : base.GetPatternCore(patternInterface);

    protected override AutomationControlType GetAutomationControlTypeCore()
        => AutomationControlType.Group;

    protected override string GetClassNameCore() => nameof(VideoThumbnailView);

    protected override string GetHelpTextCore() => _owner.AutomationAttributes();

    // A disabled slot (nothing to play) is refused before this by the bridge's actuation
    // gate; Activate's own guard covers any other UIA client.
    public void Invoke() => _owner.Activate();
}
