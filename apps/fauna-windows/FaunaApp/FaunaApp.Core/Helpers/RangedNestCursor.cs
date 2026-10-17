using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// One read cursor over the shared pinned ranged reader — the windows half of the
/// native-player leg (<c>docs/goal/architecture/security.md</c> § Transport trust →
/// <i>The native-player leg</i>; <c>render-model.md</c> § D6c → <i>Inline playback</i>,
/// answer (1)). WinUI's media stack, handed a URL, dials the nest itself and trusts the OS
/// store alone, so it refuses the self-signed floor every nest serves before ACME — the
/// same-box install's <c>https://127.0.0.1:3000</c> included. So the player is never handed
/// a URL: it reads a byte source the app owns, and every byte of it is one <c>Range</c> GET
/// through <see cref="IFfiRangedNestReader"/>, over the shared-Rust pinned client the WS
/// login armed. The <c>IRandomAccessStream</c> the player is handed
/// (<c>FaunaApp.Helpers.RangedNestStream</c>) is a WinRT shell around this; the unit-test
/// project cannot load WinRT, so every rule the stream keeps lives here, pinned by
/// <c>InlineVideoPlaybackTests</c>.
/// <para><b>Threading:</b> <see cref="Read"/> blocks on a network round trip — call it off
/// the UI thread. A cursor serializes its own reads and seeks; two cursors over one reader
/// (<see cref="Clone"/> — the media pipeline opens a second one to parse) read
/// independently, which the reader allows.</para>
/// <para><b>Lifetime:</b> a cursor never disposes its reader — the clones share it. The
/// player's owner releases it once, with the player
/// (<see cref="PlaybackMedia.Streamed.Release"/>).</para>
/// </summary>
internal sealed class RangedNestCursor
{
    private readonly IFfiRangedNestReader _reader;
    private readonly object _gate = new();
    private ulong _position;

    internal RangedNestCursor(IFfiRangedNestReader reader) : this(reader, reader.Len()) { }

    private RangedNestCursor(IFfiRangedNestReader reader, ulong? length)
    {
        _reader = reader;
        Length = length;
    }

    /// <summary>The resource's total length, or <c>null</c> when the nest named none — a
    /// proxied upstream that sent no length (render-model.md § D6c, answer 3).</summary>
    internal ulong? Length { get; }

    /// <summary>The stream's <c>Size</c>: the length, or 0 when it is unknown — WinRT has
    /// no spelling for "unknown".</summary>
    internal ulong Size => Length ?? 0;

    /// <summary>Seeking needs a known end: a resource of unknown length reads front to
    /// back.</summary>
    internal bool CanSeek => Length is not null;

    internal ulong Position
    {
        get { lock (_gate) return _position; }
    }

    /// <summary>Move the cursor. Past the end is allowed (a read there answers nothing),
    /// as on every WinRT stream; on a resource of unknown length only the current position
    /// is.</summary>
    internal void Seek(ulong position)
    {
        lock (_gate)
        {
            if (position == _position) return;
            if (!CanSeek)
                throw new NotSupportedException(
                    "the nest named no length for this resource, so it reads front to back");
            _position = position;
        }
    }

    /// <summary>
    /// Up to <paramref name="count"/> bytes from the position, which advances by what was
    /// read. Fewer only when the resource ends first; none at or past a known end, without
    /// a request. A reader answer narrower than asked is read on from where it stopped, so
    /// the player's "fill this buffer" read is never mistaken for the end of the video.
    /// Throws what the reader throws (an unreachable nest, a refused cert) — the player
    /// surfaces it as its own failure.
    /// </summary>
    internal byte[] Read(uint count)
    {
        lock (_gate)
        {
            ulong want = Length is { } length
                ? Math.Min(count, length > _position ? length - _position : 0)
                : count;
            using var read = new MemoryStream();
            while ((ulong)read.Length < want)
            {
                var chunk = _reader.ReadAt(_position, want - (ulong)read.Length);
                if (chunk.Length == 0) break;
                var take = (int)Math.Min((ulong)chunk.Length, want - (ulong)read.Length);
                read.Write(chunk, 0, take);
                _position += (ulong)take;
            }
            return read.ToArray();
        }
    }

    /// <summary>A second cursor over the same reader, at the start — what
    /// <c>IRandomAccessStream.CloneStream</c> answers.</summary>
    internal RangedNestCursor Clone() => new(_reader, Length);
}
