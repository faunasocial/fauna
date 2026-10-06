using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

// dotnet test loads the real native fauna_ffi dll, so this asserts the C#
// consumption matches the shared Rust find_overlaps (fauna_core::caltime).
public class FindOverlapsFfiTests
{
    [Fact]
    public void DisjointEventsEachGetSingleColumn()
    {
        var result = FaunaFfiMethods.FindEventOverlaps(new FfiEventInterval[]
        {
            new(startMin: 0, endMin: 60),
            new(startMin: 120, endMin: 180),
        });
        Assert.Equal(1u, result[0].totalColumns);
        Assert.Equal(1u, result[1].totalColumns);
    }

    [Fact]
    public void OverlappingEventsGetDistinctColumns()
    {
        var result = FaunaFfiMethods.FindEventOverlaps(new FfiEventInterval[]
        {
            new(startMin: 0, endMin: 120),
            new(startMin: 60, endMin: 180),
        });
        Assert.Equal(2u, result[0].totalColumns);
        Assert.NotEqual(result[0].columnIndex, result[1].columnIndex);
    }
}
