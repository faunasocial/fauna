using uniffi.fauna_core;

namespace FaunaApp.Core.Helpers;

/// <summary>A card that shows a kept inline player (<see cref="InlinePlayerKeep{TPlayer}"/>).</summary>
public interface IInlinePlayerViewer
{
    /// <summary>Stop showing the player: another card took it over, or it is about to stop.
    /// Called before the player stops, so the card's element lets go of a player that is
    /// still alive (a <c>MediaPlayerElement</c> holding a disposed player faults).</summary>
    void LetGo();
}

/// <summary>
/// The live inline players of one card list, kept apart from the cards that show them — the
/// windows form of linux's "the player outlives the card"
/// (<c>docs/goal/architecture/render-model.md</c> § D6c → <i>Inline playback</i>;
/// <c>apps/fauna-linux/src/views/feed/video_thumbnail.rs</c>).
///
/// <para>A WinUI list regenerates a row's container whenever its row moves or is replaced:
/// <c>ObservableCollection.Move</c> reaches the list as a remove and an insert. The feed moves
/// rows under the reader — a train derived while a video plays re-ranks the loaded window in
/// place (<c>engagement-cues.md</c> § Cue vocabulary &amp; derivation, <i>A play is
/// attention</i>) — so a player owned by its card went back to the idle glyph the moment its
/// post moved. Kept here instead, keyed by post and video (<see cref="InlineVideoKey"/>), the
/// card rebuilt for that post adopts it, in whichever order WinUI raises the old card's
/// <c>Unloaded</c> and the new one's <c>Loaded</c>, while another post carrying the same bytes
/// stays idle until it is tapped itself.</para>
///
/// <para>A player lives while the list holds its post with that video
/// (<see cref="Retain"/>, after every reconcile) and while the list is on screen
/// (<see cref="StopAll"/>), never by a timer: nothing plays where the reader cannot reach
/// it. Pure and UI-thread-only — <typeparamref name="TPlayer"/> is the app's player, a fake
/// in the tests.</para>
/// </summary>
public sealed class InlinePlayerKeep<TPlayer> where TPlayer : class
{
    private sealed class Entry(string post, string video, TPlayer player, IInlinePlayerViewer? viewer)
    {
        public string Post { get; } = post;
        public string Video { get; } = video;
        public TPlayer Player { get; } = player;
        public IInlinePlayerViewer? Viewer { get; set; } = viewer;
    }

    private readonly Action<TPlayer> _stop;

    /// <summary>The live players, in the order they were started — a list, so every stop
    /// happens in a stated order.</summary>
    private readonly List<Entry> _live = new();

    /// <param name="stop">Stops a player for good: the keep calls it once per player, after
    /// the player's card has let go.</param>
    public InlinePlayerKeep(Action<TPlayer> stop) => _stop = stop;

    /// <summary>How many players the keep holds.</summary>
    public int Count => _live.Count;

    /// <summary>A tap started <paramref name="player"/> on <paramref name="viewer"/>. A player
    /// the same post and video already had is stopped first.</summary>
    public void Start(string post, string video, TPlayer player, IInlinePlayerViewer viewer)
    {
        if (Find(post, video) is { } previous) StopEntry(previous);
        _live.Add(new Entry(post, video, player, viewer));
    }

    /// <summary>The card showing <paramref name="post"/>'s <paramref name="video"/> now: the
    /// live player it adopts, or <c>null</c> for none. A different card still showing it is
    /// told to let go first — the newest card for a key is the one on screen.</summary>
    public TPlayer? Attach(string post, string video, IInlinePlayerViewer viewer)
    {
        if (Find(post, video) is not { } entry) return null;
        if (entry.Viewer is { } shown && !ReferenceEquals(shown, viewer)) shown.LetGo();
        entry.Viewer = viewer;
        return entry.Player;
    }

    /// <summary>A card stops showing <paramref name="post"/>'s <paramref name="video"/> — it
    /// unloaded, or now shows another post. The player goes on unviewed, for the card rebuilt
    /// for its post to adopt; a card that was not the player's viewer changes nothing.</summary>
    public void Detach(string post, string video, IInlinePlayerViewer viewer)
    {
        if (Find(post, video) is { } entry && ReferenceEquals(entry.Viewer, viewer)) entry.Viewer = null;
    }

    /// <summary>The player of <paramref name="post"/>'s <paramref name="video"/> stopped by its
    /// own card: dropped without a second stop. A player the key no longer holds changes
    /// nothing.</summary>
    public void Forget(string post, string video, TPlayer player)
    {
        if (Find(post, video) is { } entry && ReferenceEquals(entry.Player, player)) _live.Remove(entry);
    }

    /// <summary>After the list reconciled: stop every player whose post the list no longer
    /// holds with that video. <paramref name="held"/> is every post on the list with its video
    /// key (<c>null</c> for a post with no video).</summary>
    public void Retain(IEnumerable<(string Post, string? Video)> held)
    {
        if (_live.Count == 0) return;
        var keep = new HashSet<(string, string)>();
        foreach (var (post, video) in held)
            if (video is not null) keep.Add((post, video));
        foreach (var entry in _live.ToList())
            if (!keep.Contains((entry.Post, entry.Video))) StopEntry(entry);
    }

    /// <summary>The list left the screen: stop every player.</summary>
    public void StopAll()
    {
        foreach (var entry in _live.ToList()) StopEntry(entry);
    }

    private Entry? Find(string post, string video)
        => _live.FirstOrDefault(e => e.Post == post && e.Video == video);

    private void StopEntry(Entry entry)
    {
        _live.Remove(entry);
        var viewer = entry.Viewer;
        entry.Viewer = null;
        viewer?.LetGo();
        _stop(entry.Player);
    }
}

/// <summary>The identity of the video a <c>video-thumbnail</c> slot plays, beside its post —
/// the key <see cref="InlinePlayerKeep{TPlayer}"/> keeps a player under, and the text the slot
/// paints beside its glyph.</summary>
public static class InlineVideoKey
{
    /// <summary>A blob <c>Video</c>'s hash, a bridged <c>ProxiedVideo</c>'s nest-relative path,
    /// or <c>null</c> for anything else (nothing to play). Typed <c>object</c> because the
    /// UniFFI-generated <c>RenderBlock</c> is <c>internal</c>, which XAML cannot bind.</summary>
    public static string? Of(object? block) => block switch
    {
        RenderBlock.Video video => video.hash,
        RenderBlock.ProxiedVideo proxied => proxied.path,
        _ => null,
    };
}
