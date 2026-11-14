using FaunaApp.Core.Helpers;
using uniffi.fauna_core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// A card list's live inline players, kept apart from the cards that show them
/// (<see cref="InlinePlayerKeep{TPlayer}"/>; <c>docs/goal/architecture/render-model.md</c>
/// § D6c → <i>Inline playback</i>, linux's "the player outlives the card"). A WinUI list
/// regenerates a row's container whenever the row moves — a live re-rank moves the played
/// post — so the card rebuilt for that post and video must adopt the player rather than
/// start over at the glyph, while the player still stops once nothing can show it.
/// </summary>
public class InlinePlayerKeepTests
{
    private const string Post = "post-a";
    private const string OtherPost = "post-b";
    private const string Clip = "clip-hash";
    private const string OtherClip = "other-clip-hash";

    /// <summary>Everything the keep did, in order — a stop and a let-go both land here, so
    /// a test can tell which came first.</summary>
    private readonly List<string> _log = new();

    private sealed class Player(string name)
    {
        public string Name { get; } = name;
    }

    private sealed class Card(string name, List<string> log) : IInlinePlayerViewer
    {
        public void LetGo() => log.Add($"{name} lets go");
    }

    private InlinePlayerKeep<Player> Keep() => new(p => _log.Add($"{p.Name} stopped"));

    private Card CardNamed(string name) => new(name, _log);

    [Fact]
    public void The_card_rebuilt_for_a_post_adopts_its_player_after_the_old_card_unloads()
    {
        var keep = Keep();
        var player = new Player("p");
        var oldCard = CardNamed("old");
        keep.Start(Post, Clip, player, oldCard);

        keep.Detach(Post, Clip, oldCard);
        var adopted = keep.Attach(Post, Clip, CardNamed("new"));

        Assert.Same(player, adopted);
        Assert.Empty(_log);
        Assert.Equal(1, keep.Count);
    }

    [Fact]
    public void A_rebuilt_card_loaded_before_the_old_one_unloads_takes_the_player_and_the_old_lets_go()
    {
        var keep = Keep();
        var player = new Player("p");
        var oldCard = CardNamed("old");
        var newCard = CardNamed("new");
        keep.Start(Post, Clip, player, oldCard);

        var adopted = keep.Attach(Post, Clip, newCard);
        // The old card unloads after the new one took over: the player is the new card's.
        keep.Detach(Post, Clip, oldCard);

        Assert.Same(player, adopted);
        Assert.Equal(new[] { "old lets go" }, _log);
        Assert.Same(player, keep.Attach(Post, Clip, newCard));
        Assert.Equal(new[] { "old lets go" }, _log);
    }

    [Fact]
    public void Another_post_carrying_the_same_video_stays_idle()
    {
        var keep = Keep();
        keep.Start(Post, Clip, new Player("p"), CardNamed("card"));

        Assert.Null(keep.Attach(OtherPost, Clip, CardNamed("other")));
    }

    [Fact]
    public void A_card_of_the_same_post_showing_another_video_does_not_adopt()
    {
        var keep = Keep();
        keep.Start(Post, Clip, new Player("p"), CardNamed("card"));

        Assert.Null(keep.Attach(Post, OtherClip, CardNamed("other")));
    }

    [Fact]
    public void Retain_stops_a_player_whose_post_left_the_list_or_whose_video_changed()
    {
        var keep = Keep();
        keep.Start(Post, Clip, new Player("left"), CardNamed("a"));
        keep.Start(OtherPost, Clip, new Player("changed"), CardNamed("b"));
        keep.Start("post-c", Clip, new Player("kept"), CardNamed("c"));

        keep.Retain(new (string, string?)[] { (OtherPost, OtherClip), ("post-c", Clip), ("post-d", null) });

        Assert.Equal(new[] { "a lets go", "left stopped", "b lets go", "changed stopped" }, _log);
        Assert.Equal(1, keep.Count);
    }

    [Fact]
    public void A_viewer_lets_go_before_its_player_stops_and_an_unviewed_player_just_stops()
    {
        var keep = Keep();
        var card = CardNamed("card");
        keep.Start(Post, Clip, new Player("viewed"), card);
        keep.Start(OtherPost, Clip, new Player("unviewed"), CardNamed("gone"));
        keep.Detach(OtherPost, Clip, CardNamed("gone"));
        // Detach by a card that is not the viewer changes nothing; the real one detaches:
        var gone = CardNamed("gone2");
        keep.Start("post-c", Clip, new Player("detached"), gone);
        keep.Detach("post-c", Clip, gone);

        keep.StopAll();

        Assert.Equal(
            new[] { "card lets go", "viewed stopped", "gone lets go", "unviewed stopped", "detached stopped" },
            _log);
        Assert.Equal(0, keep.Count);
    }

    [Fact]
    public void A_second_tap_on_the_same_post_and_video_replaces_the_first_player()
    {
        var keep = Keep();
        var card = CardNamed("card");
        keep.Start(Post, Clip, new Player("first"), card);

        keep.Start(Post, Clip, new Player("second"), card);

        Assert.Equal(new[] { "card lets go", "first stopped" }, _log);
        Assert.Equal("second", keep.Attach(Post, Clip, card)!.Name);
    }

    [Fact]
    public void A_player_its_card_stopped_itself_is_forgotten_without_a_second_stop()
    {
        var keep = Keep();
        var player = new Player("p");
        keep.Start(Post, Clip, player, CardNamed("card"));

        keep.Forget(Post, Clip, player);
        // A stale forget for a player the key no longer holds changes nothing.
        keep.Start(Post, Clip, new Player("next"), CardNamed("card2"));
        keep.Forget(Post, Clip, player);
        keep.StopAll();

        Assert.Equal(new[] { "card2 lets go", "next stopped" }, _log);
    }

    [Fact]
    public void A_videos_key_is_its_hash_or_its_bridged_path_and_nothing_else_has_one()
    {
        Assert.Equal("ab12", InlineVideoKey.Of(new RenderBlock.Video("ab12", "")));
        Assert.Equal("/api/v1/media/proxy?url=x", InlineVideoKey.Of(new RenderBlock.ProxiedVideo("/api/v1/media/proxy?url=x", "")));
        Assert.Null(InlineVideoKey.Of(null));
        Assert.Null(InlineVideoKey.Of("not a block"));
    }
}
