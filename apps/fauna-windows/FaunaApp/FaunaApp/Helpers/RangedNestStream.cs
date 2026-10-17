using System.Runtime.InteropServices.WindowsRuntime;
using FaunaApp.Core.Helpers;
using Windows.Foundation;
using Windows.Storage.Streams;

namespace FaunaApp.Helpers;

/// <summary>
/// The <see cref="IRandomAccessStream"/> WinUI's <c>MediaSource.CreateFromStream</c> reads a
/// nest video through: a WinRT shell over <see cref="RangedNestCursor"/>, which owns every
/// rule (and is unit-tested where WinRT cannot load). Read-only and seekable; each
/// <see cref="ReadAsync"/> is one cursor read — a pinned <c>Range</c> GET — on a pool thread,
/// never on the media pipeline's own thread or the UI's. <see cref="CloneStream"/> and
/// <see cref="GetInputStreamAt"/> answer independent cursors over the same reader, which
/// the pipeline opens to parse beside the playing stream.
/// <para>Disposing it releases nothing: the reader is shared by every clone and belongs to
/// <c>VideoThumbnailView</c>, which releases it once, with the player.</para>
/// </summary>
internal sealed class RangedNestStream : IRandomAccessStream
{
    private const string ReadOnly = "a nest video stream is read-only";

    private readonly RangedNestCursor _cursor;

    internal RangedNestStream(RangedNestCursor cursor) => _cursor = cursor;

    public bool CanRead => true;

    public bool CanWrite => false;

    public ulong Position => _cursor.Position;

    public ulong Size
    {
        get => _cursor.Size;
        set => throw new NotSupportedException(ReadOnly);
    }

    public void Seek(ulong position) => _cursor.Seek(position);

    public IAsyncOperationWithProgress<IBuffer, uint> ReadAsync(
        IBuffer buffer, uint count, InputStreamOptions options)
        => AsyncInfo.Run<IBuffer, uint>((ct, _) => Task.Run(() =>
        {
            // Into the caller's buffer, as the contract asks: never past its capacity.
            var bytes = _cursor.Read(Math.Min(count, buffer.Capacity));
            bytes.CopyTo(0, buffer, 0, bytes.Length);
            buffer.Length = (uint)bytes.Length;
            return buffer;
        }, ct));

    public IRandomAccessStream CloneStream() => new RangedNestStream(_cursor.Clone());

    public IInputStream GetInputStreamAt(ulong position)
    {
        var cursor = _cursor.Clone();
        cursor.Seek(position);
        return new RangedNestStream(cursor);
    }

    public IOutputStream GetOutputStreamAt(ulong position) => throw new NotSupportedException(ReadOnly);

    public IAsyncOperationWithProgress<uint, uint> WriteAsync(IBuffer buffer)
        => throw new NotSupportedException(ReadOnly);

    // Nothing is ever written, so there is nothing to flush.
    public IAsyncOperation<bool> FlushAsync() => AsyncInfo.Run(_ => Task.FromResult(true));

    public void Dispose() { }
}
