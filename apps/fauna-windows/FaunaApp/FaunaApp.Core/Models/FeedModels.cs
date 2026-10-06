namespace FaunaApp.Core.Models;

/// Public XAML-binding wrapper for a feed-selector row (`feed-item`), built from
/// the shared snapshot's <c>FeedSummaryView</c> (internal UniFFI record). A
/// public type so the feed-rail DataTemplate's <c>x:DataType</c> resolves at
/// runtime — the same materialization constraint as the post-card wrapper. The
/// authoritative feed list lives in the shared <c>FeedManager</c> snapshot; this
/// is a per-tick render projection, not state.
public sealed record FeedDefinition
{
    // Plain settable properties (not `init`): the WinUI XAML type-info generator
    // emits a post-construction setter for every public property of a DataTemplate
    // `x:DataType`, and an `init`-only accessor can't be assigned there (CS8852).
    public string FeedId { get; set; } = "";
    public string Name { get; set; } = "";
    public string Combination { get; set; } = "all";
}
