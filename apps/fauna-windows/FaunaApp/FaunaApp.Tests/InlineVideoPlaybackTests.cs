using System.Globalization;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using uniffi.fauna_core;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The windows inline player's pure half (<c>docs/goal/architecture/render-model.md</c>
/// § D6c → <i>Inline playback</i>): the phase walk the <c>video-thumbnail</c> element
/// publishes as <c>state</c>, the attributes it packs into its HelpText, and the turn of
/// the shared <c>FeedManager::playback_source</c> decision into what
/// <c>MediaPlayerElement</c> is handed, and the read cursor its nest streams read through
/// (<see cref="RangedNestCursor"/>). The WinUI half (<c>VideoThumbnailView</c>,
/// <c>RangedNestStream</c>) needs a UI thread, WinRT and a media pipeline, so everything it
/// decides lives here, where it can be pinned headlessly — the twin of apple's
/// <c>InlineVideoPlaybackTests</c> and web's <c>VideoThumbnail.svelte</c> <c>playState</c>.
/// </summary>
public class InlineVideoPlaybackTests
{
    private const string Hash = "ab" + "00000000000000000000000000000000000000000000000000000000000000";
    private const string BlobPath = "/api/v1/blob/" + Hash;
    private static readonly RenderBlock BlobVideo = new RenderBlock.Video(Hash, "");

    // A bridged post's video: the proxy's nest-relative path as the fold rests it, and
    // the same path once `playback_source` has minted its playback ticket (answer 4).
    private const string ProxiedClip = "/api/v1/media/proxy?url=https%3A%2F%2Fmedia.test%2Fclip.mp4";
    private const string TicketedClip = ProxiedClip + "&exp=1760000000&sig=Zm9vYmFyYmF6-_0";
    private static readonly RenderBlock BridgedVideo = new RenderBlock.ProxiedVideo(ProxiedClip, "");

    /// <summary>A public video's stream, as the resolver hands it over.</summary>
    private static PlaybackMedia.Streamed StreamedBlob()
        => new(BlobPath, "video/mp4", new FakeReader(Array.Empty<byte>()));

    /// <summary>
    /// The shared ranged reader over an in-memory resource: answers like the Rust reader
    /// (up to <c>maxLen</c> at <c>offset</c>, shorter at the end, empty past it) and
    /// records every read, each of which is a <c>Range</c> GET in the real one.
    /// </summary>
    private sealed class FakeReader : FfiRangedNestReaderFakeBase, IDisposable
    {
        private readonly byte[] _data;
        private readonly bool _knownLength;
        private readonly string? _contentType;

        public FakeReader(byte[] data, string? contentType = "video/mp4", bool knownLength = true)
        {
            _data = data;
            _contentType = contentType;
            _knownLength = knownLength;
        }

        /// <summary>Answer at most this many bytes per read — a ranged reply narrower than
        /// asked.</summary>
        public int? MaxChunk { get; init; }

        public List<(ulong Offset, ulong MaxLen)> Reads { get; } = new();

        public bool Disposed { get; private set; }

        public override ulong? Len() => _knownLength ? (ulong)_data.Length : null;

        public override string? ContentType() => _contentType;

        public override byte[] ReadAt(ulong offset, ulong maxLen)
        {
            Reads.Add((offset, maxLen));
            if (offset >= (ulong)_data.Length) return Array.Empty<byte>();
            var n = (int)Math.Min(maxLen, (ulong)_data.Length - offset);
            if (MaxChunk is { } cap) n = Math.Min(n, cap);
            return _data.AsSpan((int)offset, n).ToArray();
        }

        public void Dispose() => Disposed = true;
    }

    /// <summary><c>count</c> bytes counting up from 0, so a slice names its offset.</summary>
    private static byte[] Counting(int count) => Enumerable.Range(0, count).Select(i => (byte)i).ToArray();

    // ── The phase walk ──

    [Fact]
    public void A_video_starts_idle_and_never_leaves_it_by_itself()
    {
        var machine = new VideoPlaybackMachine();
        Assert.Equal(VideoPlaybackPhase.Idle, machine.Phase);
        Assert.Null(machine.Media);
        // A player event with nothing tapped is not a playback: never autoplay.
        machine.PlayerStartedPlaying();
        machine.PlayerFailed();
        Assert.Equal(VideoPlaybackPhase.Idle, machine.Phase);
    }

    [Fact]
    public void The_tap_starts_a_resolve_and_a_url_is_not_playback_yet()
    {
        var machine = new VideoPlaybackMachine();
        Assert.True(machine.Tap());
        Assert.Equal(VideoPlaybackPhase.Loading, machine.Phase);

        machine.Resolved(StreamedBlob());
        // Buffering: still loading until the player itself says it is playing.
        Assert.Equal(VideoPlaybackPhase.Loading, machine.Phase);

        machine.PlayerStartedPlaying();
        Assert.Equal(VideoPlaybackPhase.Playing, machine.Phase);
    }

    [Fact]
    public void Nothing_playable_is_an_error_and_a_tap_retries_it()
    {
        var machine = new VideoPlaybackMachine();
        machine.Tap();
        machine.Resolved(null);
        Assert.Equal(VideoPlaybackPhase.Error, machine.Phase);

        Assert.True(machine.Tap());
        Assert.Equal(VideoPlaybackPhase.Loading, machine.Phase);
        Assert.Null(machine.Media);
    }

    [Fact]
    public void A_tap_while_loading_or_playing_belongs_to_the_player_controls()
    {
        var machine = new VideoPlaybackMachine();
        machine.Tap();
        Assert.False(machine.Tap());
        machine.Resolved(StreamedBlob());
        machine.PlayerStartedPlaying();
        Assert.False(machine.Tap());
        Assert.Equal(VideoPlaybackPhase.Playing, machine.Phase);
    }

    [Fact]
    public void The_players_own_failure_is_the_error_state()
    {
        var machine = new VideoPlaybackMachine();
        machine.Tap();
        machine.Resolved(StreamedBlob());
        machine.PlayerFailed();
        Assert.Equal(VideoPlaybackPhase.Error, machine.Phase);
    }

    [Fact]
    public void A_second_resolve_for_the_same_tap_is_dropped()
    {
        var machine = new VideoPlaybackMachine();
        machine.Tap();
        machine.Resolved(StreamedBlob());
        machine.Resolved(null);
        Assert.Equal(VideoPlaybackPhase.Loading, machine.Phase);
        Assert.NotNull(machine.Media);
    }

    [Fact]
    public void Reset_returns_to_the_glyph()
    {
        var machine = new VideoPlaybackMachine();
        machine.Tap();
        machine.Resolved(StreamedBlob());
        machine.PlayerStartedPlaying();
        machine.Reset();
        Assert.Equal(VideoPlaybackPhase.Idle, machine.Phase);
        Assert.Null(machine.Media);
    }

    // ── What the element publishes ──

    [Fact]
    public void An_idle_thumbnail_publishes_idle_with_no_position_and_no_source()
        => Assert.Equal("state=idle;position=0.000;source=",
            new VideoPlaybackMachine().AutomationAttributes(0));

    [Fact]
    public void A_playing_stream_publishes_its_phase_position_and_nest_relative_path()
    {
        // The path, not a URL: no URL was handed to anything (render-model.md § D6c →
        // Inline playback, answer 1 — the `source` attribute stays the nest-relative path).
        var machine = new VideoPlaybackMachine();
        machine.Tap();
        machine.Resolved(StreamedBlob());
        machine.PlayerStartedPlaying();
        Assert.Equal($"state=playing;position=0.420;source={BlobPath}",
            machine.AutomationAttributes(0.42));
    }

    [Fact]
    public void The_position_is_culture_invariant()
    {
        // A comma decimal separator would make the driver's float() parse fail on a
        // de-DE desktop — the published attribute is a wire value, never a display one.
        var saved = CultureInfo.CurrentCulture;
        try
        {
            CultureInfo.CurrentCulture = new CultureInfo("de-DE");
            Assert.Contains("position=1.500;",
                new VideoPlaybackMachine().AutomationAttributes(1.5));
        }
        finally
        {
            CultureInfo.CurrentCulture = saved;
        }
    }

    [Fact]
    public void A_sealed_video_names_its_hash_as_the_source()
    {
        // No URL can serve a sealed item's plaintext: the opened bytes play from memory,
        // so the source names what was opened rather than a route.
        var machine = new VideoPlaybackMachine();
        machine.Tap();
        machine.Resolved(new PlaybackMedia.Opened(Hash, new byte[] { 1 }, "video/mp4"));
        Assert.EndsWith($";source=sealed:{Hash}", machine.AutomationAttributes(0));
    }

    [Fact]
    public void A_packed_value_never_carries_the_pair_separator()
        // The bridge unpacks on ';' — a value holding one would split into a bogus pair —
        // and '%' is escaped first, so a literal "%3B" in a value survives the round trip.
        => Assert.Equal("a=x%3By;b=50%25;c=%253B",
            AutomationAttributePack.Pack(("a", "x;y"), ("b", "50%"), ("c", "%3B")));

    // ── Resolving what plays (the shared decision → the player's input) ──

    /// <summary>Answers a scripted <c>playback_source</c> decision and records the block it
    /// was asked about; opens sealed bytes with a scripted verdict.</summary>
    private sealed class FakeFeedManager : FfiFeedManagerFakeBase
    {
        public uniffi.fauna_feed.PlaybackSource Answer =
            new uniffi.fauna_feed.PlaybackSource.Unplayable("unset");
        public RenderBlock? AskedAbout;
        public byte[]? OpenVerdict;
        public string? OpenedHash;

        public override Task<uniffi.fauna_feed.PlaybackSource> PlaybackSource(RenderBlock block)
        {
            AskedAbout = block;
            return Task.FromResult(Answer);
        }

        public override byte[]? OpenMediaBytes(string blobHash, byte[] fetched)
        {
            OpenedHash = blobHash;
            return OpenVerdict;
        }
    }

    private static MockNestHttpClient Nest(byte[]? blob = null) => new()
    {
        NestUrl = "https://127.0.0.1:3000",
        NextBlobBytes = blob ?? Array.Empty<byte>(),
    };

    /// <summary>Stands in for <c>FfiRangedNestReader.Open</c>: records what it was asked to
    /// open and answers <see cref="Reader"/>.</summary>
    private sealed class Opener
    {
        public FakeReader Reader { get; init; } = new(Array.Empty<byte>());
        public List<(string NestUrl, string Path)> Opened { get; } = new();

        public IFfiRangedNestReader Open(string nestUrl, string path)
        {
            Opened.Add((nestUrl, path));
            return Reader;
        }
    }

    [Fact]
    public async Task The_manager_is_asked_about_the_tapped_videos_own_block()
    {
        var manager = new FakeFeedManager();
        await VideoPlaybackResolver.ResolveAsync(Nest(), manager, BlobVideo);
        var video = Assert.IsType<RenderBlock.Video>(manager.AskedAbout);
        Assert.Equal(Hash, video.hash);
    }

    [Fact]
    public async Task A_bridged_video_asks_about_its_own_ProxiedVideo_block()
    {
        // The ticket is minted inside the shared decision (render-model.md § D6c → Inline
        // playback, answer 4), so the slot hands the manager the folded block itself — never
        // a path it built a URL from by hand.
        var manager = new FakeFeedManager();
        await VideoPlaybackResolver.ResolveAsync(Nest(), manager, BridgedVideo);
        Assert.Equal(BridgedVideo, manager.AskedAbout);
    }

    [Fact]
    public async Task A_ticketed_url_reaches_the_player_with_its_exp_and_sig_intact()
    {
        // The ticket is the player's only credential on the proxy: it cannot carry the
        // session bearer. A query dropped or re-encoded on the way to the player is a 401
        // the reader sees as a dead thumbnail — so what the stream carries, and the
        // `source` the element publishes, keep the ticketed path whole.
        var manager = new FakeFeedManager { Answer = new PlaybackSource.Url(TicketedClip) };
        var opener = new Opener();

        var media = await VideoPlaybackResolver.ResolveAsync(
            Nest(), manager, BridgedVideo, openReader: opener.Open);

        var streamed = Assert.IsType<PlaybackMedia.Streamed>(media);
        Assert.Equal(TicketedClip, Assert.Single(opener.Opened).Path);
        Assert.EndsWith(TicketedClip, streamed.Source);
    }

    [Fact]
    public async Task A_url_answer_opens_the_pinned_reader_on_this_nest()
    {
        // security.md § Transport trust → The native-player leg: the player reads bytes
        // through the shared pinned reader, opened on the URL the login minted on (the pin
        // store's key) + the projection's path — never a URL its own stack would dial.
        var manager = new FakeFeedManager { Answer = new PlaybackSource.Url(BlobPath) };
        var nest = Nest();
        var opener = new Opener();

        var media = await VideoPlaybackResolver.ResolveAsync(nest, manager, BlobVideo, openReader: opener.Open);

        var streamed = Assert.IsType<PlaybackMedia.Streamed>(media);
        Assert.Equal(("https://127.0.0.1:3000", BlobPath), Assert.Single(opener.Opened));
        Assert.Same(opener.Reader, streamed.Reader);
        Assert.Equal(BlobPath, streamed.Path);
        Assert.Equal("video/mp4", streamed.ContentType);
        // The stream reads on demand: the resolve pulled no whole blob through the app.
        Assert.DoesNotContain("GetBlob", nest.Calls);
    }

    [Fact]
    public async Task A_url_that_is_not_nest_relative_is_not_played()
    {
        // The projection only ever answers a rooted path; anything else names another
        // origin, and nothing is dialed for it.
        var manager = new FakeFeedManager { Answer = new PlaybackSource.Url("//elsewhere.example/x") };
        var opener = new Opener();
        Assert.Null(await VideoPlaybackResolver.ResolveAsync(Nest(), manager, BlobVideo, openReader: opener.Open));
        Assert.Empty(opener.Opened);
    }

    [Fact]
    public async Task A_reader_that_does_not_open_is_nothing_playable_not_a_crash()
    {
        // An unreachable nest, or a cert neither WebPKI-valid nor the pin: the element shows
        // `error`, and the next tap retries.
        var manager = new FakeFeedManager { Answer = new PlaybackSource.Url(BlobPath) };
        Assert.Null(await VideoPlaybackResolver.ResolveAsync(
            Nest(), manager, BlobVideo,
            openReader: (_, _) => throw new InvalidOperationException("cert refused")));
    }

    [Fact]
    public async Task A_declared_video_type_is_what_the_stream_plays_as()
    {
        // A proxied fediverse clip may be WebM: its brand must not be guessed as MP4.
        var manager = new FakeFeedManager { Answer = new PlaybackSource.Url(TicketedClip) };
        var opener = new Opener { Reader = new FakeReader(Counting(32), "video/webm; codecs=vp9") };

        var media = await VideoPlaybackResolver.ResolveAsync(
            Nest(), manager, BridgedVideo, openReader: opener.Open);

        Assert.Equal("video/webm", Assert.IsType<PlaybackMedia.Streamed>(media).ContentType);
        Assert.Empty(opener.Reader.Reads);
    }

    [Fact]
    public async Task An_undeclared_type_is_read_off_the_first_bytes()
    {
        // The blob route answers `application/octet-stream` for an upload whose stored type
        // was not an allowlisted one: the brand decides, read through the reader.
        var qt = new byte[] { 0, 0, 0, 0x14, (byte)'f', (byte)'t', (byte)'y', (byte)'p',
            (byte)'q', (byte)'t', (byte)' ', (byte)' ', 9, 9 };
        var manager = new FakeFeedManager { Answer = new PlaybackSource.Url(BlobPath) };
        var opener = new Opener { Reader = new FakeReader(qt, "application/octet-stream") };

        var media = await VideoPlaybackResolver.ResolveAsync(Nest(), manager, BlobVideo, openReader: opener.Open);

        Assert.Equal("video/quicktime", Assert.IsType<PlaybackMedia.Streamed>(media).ContentType);
        Assert.Equal(((ulong)0, (ulong)12), Assert.Single(opener.Reader.Reads));
    }

    [Fact]
    public void Releasing_a_stream_closes_its_reader()
    {
        var reader = new FakeReader(Counting(4));
        new PlaybackMedia.Streamed(BlobPath, "video/mp4", reader).Release();
        Assert.True(reader.Disposed);
    }

    [Fact]
    public async Task A_sealed_answer_fetches_and_opens_the_blob_and_plays_the_plaintext()
    {
        var opened = new byte[] { 0, 0, 0, 0x18, (byte)'f', (byte)'t', (byte)'y', (byte)'p',
            (byte)'i', (byte)'s', (byte)'o', (byte)'m' };
        var manager = new FakeFeedManager { Answer = new PlaybackSource.Sealed(Hash), OpenVerdict = opened };
        var nest = Nest(blob: new byte[] { 0xAE, 0xAD });

        var media = await VideoPlaybackResolver.ResolveAsync(nest, manager, BlobVideo);

        var sealedMedia = Assert.IsType<PlaybackMedia.Opened>(media);
        Assert.Equal(Hash, manager.OpenedHash);
        Assert.Equal(opened, sealedMedia.Bytes);
        Assert.Equal("video/mp4", sealedMedia.ContentType);
        Assert.Equal(Hash, sealedMedia.Hash);
    }

    [Fact]
    public async Task A_sealed_item_that_does_not_open_never_reaches_the_player()
    {
        // Ciphertext handed to a decoder looks identical to a corrupt file and hides a
        // real key failure — the same rule the sealed image keeps (PostMediaOpen).
        var manager = new FakeFeedManager { Answer = new PlaybackSource.Sealed(Hash), OpenVerdict = null };
        Assert.Null(await VideoPlaybackResolver.ResolveAsync(Nest(blob: new byte[] { 0xAE }), manager, BlobVideo));
    }

    [Fact]
    public async Task A_failed_fetch_is_nothing_playable_not_a_crash()
    {
        var manager = new FakeFeedManager { Answer = new PlaybackSource.Sealed(Hash), OpenVerdict = new byte[] { 1 } };
        var nest = Nest();
        nest.NextError = "503";
        Assert.Null(await VideoPlaybackResolver.ResolveAsync(nest, manager, BlobVideo));
    }

    [Fact]
    public async Task Unplayable_resolves_to_nothing()
    {
        var manager = new FakeFeedManager { Answer = new PlaybackSource.Unplayable("not a video block") };
        Assert.Null(await VideoPlaybackResolver.ResolveAsync(Nest(), manager, BlobVideo));
    }

    [Fact]
    public async Task No_manager_or_no_nest_resolves_to_nothing()
    {
        Assert.Null(await VideoPlaybackResolver.ResolveAsync(Nest(), null, BlobVideo));
        Assert.Null(await VideoPlaybackResolver.ResolveAsync(null, new FakeFeedManager(), BlobVideo));
    }

    [Fact]
    public void A_quicktime_brand_plays_as_quicktime_everything_else_as_mp4()
    {
        // The `Video` block carries no mime by design (the image-vs-video branch is made once,
        // in the shared fold), so the in-memory stream's content type is read off the
        // ISO-BMFF brand — apple's SealedPlaybackFile.fileExtension rule.
        var qt = new byte[] { 0, 0, 0, 0x14, (byte)'f', (byte)'t', (byte)'y', (byte)'p',
            (byte)'q', (byte)'t', (byte)' ', (byte)' ' };
        Assert.Equal("video/quicktime", VideoPlaybackResolver.ContentTypeOf(qt));
        Assert.Equal("video/mp4", VideoPlaybackResolver.ContentTypeOf(new byte[] { 1, 2, 3 }));
    }

    // ── The nest the reader opens on ──

    [Fact]
    public void The_reader_opens_on_the_url_the_client_was_configured_with()
        // The string the login minted on, trailing slash trimmed as everywhere the client
        // uses it — so the reader's pin-store key is the one the WS handshake wrote.
        => Assert.Equal("https://127.0.0.1:3000",
            new DirectNestClient("https://127.0.0.1:3000/", new MockCryptoService()).NestUrl);

    // ── The read cursor a nest stream reads through ──

    [Fact]
    public void A_read_takes_the_bytes_at_the_position_and_advances_it()
    {
        var reader = new FakeReader(Counting(100));
        var cursor = new RangedNestCursor(reader);

        Assert.Equal(Counting(10), cursor.Read(10));
        Assert.Equal((ulong)10, cursor.Position);
        Assert.Equal(new byte[] { 10, 11, 12, 13, 14 }, cursor.Read(5));
        Assert.Equal(new[] { ((ulong)0, (ulong)10), ((ulong)10, (ulong)5) }, reader.Reads);
    }

    [Fact]
    public void A_seek_moves_the_next_read()
    {
        var cursor = new RangedNestCursor(new FakeReader(Counting(100)));
        Assert.Equal((ulong)100, cursor.Size);
        Assert.True(cursor.CanSeek);

        cursor.Seek(50);

        Assert.Equal(new byte[] { 50, 51, 52, 53 }, cursor.Read(4));
    }

    [Fact]
    public void A_read_at_the_end_is_short_and_past_it_asks_the_nest_nothing()
    {
        var reader = new FakeReader(Counting(20));
        var cursor = new RangedNestCursor(reader);
        cursor.Seek(16);

        Assert.Equal(new byte[] { 16, 17, 18, 19 }, cursor.Read(10));
        Assert.Equal((ulong)20, cursor.Position);
        // Clamped to the known length: the request asked for the 4 bytes that exist.
        Assert.Equal(((ulong)16, (ulong)4), Assert.Single(reader.Reads));

        Assert.Empty(cursor.Read(10));
        cursor.Seek(500);
        Assert.Empty(cursor.Read(10));
        Assert.Single(reader.Reads);
    }

    [Fact]
    public void A_narrow_answer_is_read_on_until_the_request_is_filled()
    {
        // A ranged reply narrower than asked is not the end of the video: the player's
        // "fill this buffer" read gets every byte it asked for.
        var reader = new FakeReader(Counting(100)) { MaxChunk = 3 };
        var cursor = new RangedNestCursor(reader);

        Assert.Equal(Counting(10), cursor.Read(10));
        Assert.Equal(new ulong[] { 0, 3, 6, 9 }, reader.Reads.Select(r => r.Offset));
        Assert.Equal((ulong)10, cursor.Position);
    }

    [Fact]
    public void A_clone_reads_the_same_resource_from_its_own_start()
    {
        // The media pipeline parses through a second stream while the first plays.
        var reader = new FakeReader(Counting(100));
        var playing = new RangedNestCursor(reader);
        playing.Read(10);

        var parsing = playing.Clone();

        Assert.Equal((ulong)0, parsing.Position);
        Assert.Equal((ulong)100, parsing.Size);
        Assert.Equal(new byte[] { 0, 1, 2 }, parsing.Read(3));
        Assert.Equal(new byte[] { 10, 11 }, playing.Read(2));
        Assert.Equal((ulong)3, parsing.Position);
    }

    [Fact]
    public void An_unknown_length_reports_size_zero_and_reads_front_to_back()
    {
        // A proxied upstream that named no length: WinRT has no "unknown" size, and with no
        // end to seek from the resource reads in order.
        var reader = new FakeReader(Counting(8), knownLength: false);
        var cursor = new RangedNestCursor(reader);

        Assert.Null(cursor.Length);
        Assert.Equal((ulong)0, cursor.Size);
        Assert.False(cursor.CanSeek);
        Assert.Equal(Counting(5), cursor.Read(5));
        cursor.Seek(5);
        Assert.Throws<NotSupportedException>(() => cursor.Seek(0));
        Assert.Equal(new byte[] { 5, 6, 7 }, cursor.Read(5));
        Assert.Empty(cursor.Read(5));
    }

    [Fact]
    public void A_failed_read_reaches_the_player_and_leaves_the_position()
    {
        // An unreachable nest mid-play is the player's own failure (`error`), never a
        // silent end of stream.
        var cursor = new RangedNestCursor(new ThrowingReader());
        Assert.Throws<InvalidOperationException>(() => cursor.Read(4));
        Assert.Equal((ulong)0, cursor.Position);
    }

    private sealed class ThrowingReader : FfiRangedNestReaderFakeBase
    {
        public override ulong? Len() => 100;
        public override byte[] ReadAt(ulong offset, ulong maxLen) => throw new InvalidOperationException("nest gone");
    }
}
