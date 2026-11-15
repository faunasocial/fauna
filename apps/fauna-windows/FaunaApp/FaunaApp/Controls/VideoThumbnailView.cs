using FaunaApp.Core.Helpers;
using FaunaApp.Helpers;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
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
/// <para><b>The player outlives the card</b> — linux's shape
/// (<c>apps/fauna-linux/src/views/feed/video_thumbnail.rs</c>). The playback itself — the
/// resolve, the player, its stream, the phase — is an <see cref="InlineVideoSession"/> this
/// view shows. A list card's session is kept by its list
/// (<see cref="Feed.PostCardHost.Players"/>), keyed by post and video: WinUI regenerates a
/// row's container whenever the feed moves the row (a live re-rank — <c>engagement-cues.md</c>
/// § <i>A play is attention</i>), and the card rebuilt for that post adopts the player, still
/// playing, instead of starting over at the glyph. The list stops it once it no longer holds
/// the post with that video, or leaves the screen. The post-detail dialog's slot sits in no
/// list: its session is its own, stopped when the slot unloads.</para>
///
/// <para><b>What the drivers read</b> (<c>drivers/base.py::get_attr</c>): <c>state</c>
/// (<c>idle</c> / <c>loading</c> / <c>playing</c> / <c>error</c>), <c>position</c> (the
/// player's seconds) and <c>source</c> (what it was handed). UIA carries one
/// application-supplied string per element, so the automation peer packs all three into
/// its HelpText (<see cref="AutomationAttributePack"/>) and the bridge answers each key on
/// its own. The peer computes the string AT READ TIME off the live player, so the position
/// a test polls is the player's own and no timer republishes it. The phase walk itself is
/// the pure <see cref="VideoPlaybackMachine"/>, pinned by <c>InlineVideoPlaybackTests</c>,
/// and the keep the pure <see cref="InlinePlayerKeep{TPlayer}"/>, pinned by
/// <c>InlinePlayerKeepTests</c>; this control is only the glue.</para>
///
/// <para><b>Threading:</b> UI thread only — the session raises its changes there, which is
/// also where UIA calls the peer, so the published phase is never read mid-write.</para>
/// </summary>
public sealed class VideoThumbnailView : UserControl, IInlinePlayerViewer
{
    private readonly Grid _slot = new();
    private readonly StackPanel _row = new() { Orientation = Orientation.Horizontal, Spacing = 4 };
    private readonly TextBlock _glyph = new()
    {
        Text = "",
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

    /// <summary>What an idle slot publishes: no session, nothing tapped.</summary>
    private static readonly VideoPlaybackMachine IdleMachine = new();

    private RenderBlock? _block;
    private string? _postId;

    /// <summary>The playback this slot shows, while it shows one.</summary>
    private InlineVideoSession? _session;

    /// <summary>The element showing <see cref="_session"/>'s player, while it has one —
    /// detached from the player before the player is disposed (the session's
    /// <c>ReleasePlayer</c>).</summary>
    private MediaPlayerElement? _element;

    /// <summary>The card list this slot sits in, found when it loads — <c>null</c> for the
    /// post-detail dialog, whose session is its own.</summary>
    private Feed.PostCardHost? _host;

    private bool _loaded;

    public VideoThumbnailView()
    {
        _row.Children.Add(_glyph);
        _row.Children.Add(_spinner);
        _row.Children.Add(_label);
        _button.Click += (_, _) => Activate();
        Content = _slot;
        Loaded += (_, _) =>
        {
            _loaded = true;
            _host = Feed.PostCardHost.Of(this);
            AdoptKept();
        };
        // A slot leaving the tree lets go of what it shows. A list card's player goes on, for
        // the card rebuilt for its post to adopt — the list stops it once nothing can show it;
        // the dialog's player stops with its slot: nothing plays where the reader cannot see
        // it, and the decoder's memory goes with it.
        Unloaded += (_, _) =>
        {
            _loaded = false;
            LeaveSession();
        };
        Paint();
    }

    /// <summary>The block this slot plays — a <c>RenderBlock.Video</c> or
    /// <c>RenderBlock.ProxiedVideo</c>, or <c>null</c> for nothing to play. Typed
    /// <c>object</c> because the UniFFI-generated <c>RenderBlock</c> is <c>internal</c>, which
    /// the XAML compiler cannot bind; any other value is nothing to play. Setting it paints
    /// the text beside the glyph — the hash, or the nest-relative path — which is also the
    /// element's accessible name (a DataTemplate element with no name is pruned from UIA, so
    /// FlaUI would count 0). A new block — a recycled row now showing another post —
    /// leaves the old one's playback and shows the new one's, if its list keeps one.</summary>
    public object? Block
    {
        get => _block;
        set
        {
            var next = value as RenderBlock;
            if (Equals(next, _block)) return;
            LeaveSession();
            _block = next;
            var text = InlineVideoKey.Of(next) ?? "";
            _label.Text = text;
            AutomationProperties.SetName(this, text);
            AdoptKept();
            Paint();
        }
    }

    /// <summary>The post a list card's slot belongs to — the key its list keeps the player
    /// under, beside the video, and the post its session reports progress and engagement for
    /// to the engagement-cue capture (<see cref="Feed.CueViewportObserver.NotePlayed"/>): a
    /// video played to its end derives the media <c>watch-complete</c>
    /// (<c>engagement-cues.md</c> § Cue vocabulary &amp; derivation). Left <c>null</c> by the
    /// post-detail dialog, whose player plays outside the list the capture samples.</summary>
    public string? PostId
    {
        get => _postId;
        set
        {
            if (value == _postId) return;
            LeaveSession();
            _postId = value;
            AdoptKept();
            Paint();
        }
    }

    /// <summary>The post's shared <c>post_blob_home</c> (<c>FfiPostBlobHome</c>, typed
    /// <c>object</c> for the same reason as <see cref="Block"/>), handed unchanged to the
    /// post-blob plane with a sealed video's hash, or a plain one's when the post is homed
    /// on another nest; <c>null</c> = this nest.</summary>
    public object? Home { get; set; }

    /// <summary>The video element's settled label (<c>PostMediaElement.Alt</c> — the author's
    /// alt, else the shared <c>feed.post.video_alt_fallback</c> text), published as the
    /// element's <c>alt</c> attribute beside <c>state</c>/<c>position</c>/<c>source</c>
    /// (render-model.md § D6c → <i>Proxied video</i>, the <c>alt</c> paragraph). <c>null</c>
    /// publishes no <c>alt</c>.</summary>
    public string? Alt { get; set; }

    /// <summary>The keep this slot's playback lives in, and its key — only for a list card
    /// with a post and something to play.</summary>
    private (InlinePlayerKeep<InlineVideoSession> Keep, string Post, string Video)? Kept
        => _host?.Players is { } keep && _postId is { Length: > 0 } post && InlineVideoKey.Of(_block) is { } video
            ? (keep, post, video)
            : null;

    /// <summary><c>state;position;source</c> (and <c>alt</c>, when the slot has one), packed —
    /// what the automation peer answers as HelpText. Read on the UI thread (UIA's provider
    /// calls are marshalled there).</summary>
    internal string AutomationAttributes()
    {
        var playback = _session?.AutomationAttributes() ?? IdleMachine.AutomationAttributes(0);
        return string.IsNullOrEmpty(Alt)
            ? playback
            : $"{playback};{AutomationAttributePack.Pack(("alt", Alt))}";
    }

    /// <summary>The tap — the button, or UIA's Invoke. Starts a session when the slot shows
    /// none (kept by the list for a list card), then hands it the tap.</summary>
    internal void Activate()
    {
        if (_block is not { } block) return;
        if (_session is null)
        {
            var session = new InlineVideoSession(_postId, DispatcherQueue);
            if (Kept is { } kept) kept.Keep.Start(kept.Post, kept.Video, session, this);
            Show(session);
        }
        _session!.Activate(block, Home as uniffi.fauna_ffi.FfiPostBlobHome);
    }

    /// <summary>Another card took this slot's player over, or the list is stopping it:
    /// stop showing it (<see cref="IInlinePlayerViewer"/>).</summary>
    void IInlinePlayerViewer.LetGo()
    {
        Trace("lets go of its player");
        Show(null);
    }

    /// <summary>A loaded list card shows the player its list keeps for its post and video,
    /// if one is live.</summary>
    private void AdoptKept()
    {
        if (!_loaded || _session is not null || Kept is not { } kept) return;
        if (kept.Keep.Attach(kept.Post, kept.Video, this) is not { } session) return;
        Trace($"adopts its kept player ({session.Phase})");
        Show(session);
    }

    /// <summary>Stop showing the session: a kept one goes on unviewed, for the card rebuilt
    /// for its post; the dialog's own stops.</summary>
    private void LeaveSession()
    {
        if (_session is not { } session) return;
        var kept = Kept;
        Show(null);
        if (kept is { } k)
        {
            Trace($"leaves its player playing on ({session.Phase}), for the card rebuilt for its post");
            k.Keep.Detach(k.Post, k.Video, this);
        }
        else session.Stop();
    }

    /// <summary>One line of the e2e trace (<c>FAUNA_E2E_AGENT_LOG</c>; compiled out of
    /// release builds) about this list card's player hand-off.</summary>
    private void Trace(string what)
    {
        var post = _postId is { } id ? id[..Math.Min(12, id.Length)] : "";
        Core.Logs.E2eTrace.Write($"[video] card of post={post} {what}");
    }

    private void Show(InlineVideoSession? session)
    {
        if (ReferenceEquals(session, _session)) return;
        if (_session is { } previous) previous.Changed -= Paint;
        _session = session;
        if (session is not null) session.Changed += Paint;
        Paint();
    }

    /// <summary>The slot as the session stands: its player's element while it has a player,
    /// else the glyph (a spinner while resolving) + the text, inside a button when there is
    /// something to play, bare when there is not. An idle slot with nothing to play is
    /// disabled, so a tap on it is the named refusal rather than a silent no-op.</summary>
    private void Paint()
    {
        var player = _session?.Player;
        if (_element is { } shown && !ReferenceEquals(shown.MediaPlayer, player))
        {
            // The element lets go of the player, and leaves the slot, BEFORE the player is
            // disposed — the session tells this view (Changed) before it disposes one.
            _element = null;
            shown.SetMediaPlayer(null);
            _slot.Children.Remove(shown);
        }
        if (player is not null)
        {
            if (_element is not null) return;
            var element = new MediaPlayerElement
            {
                AreTransportControlsEnabled = true,
                AutoPlay = false,
                Height = 200,
                HorizontalAlignment = HorizontalAlignment.Left,
                Stretch = Stretch.Uniform,
            };
            element.SetMediaPlayer(player);
            _element = element;
            _slot.Children.Clear();
            _slot.Children.Add(element);
            IsEnabled = true;
            return;
        }

        var loading = _session?.Phase == VideoPlaybackPhase.Loading;
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
