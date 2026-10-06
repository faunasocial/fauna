using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using FaunaApp.Core.Services;
using uniffi.fauna_client_search;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Thin observer over the shared <see cref="FfiSearchManager"/> — the Search
/// page's analogue of <see cref="FeedViewModel"/>. All query / type-filter /
/// paging / local-nest-merge state lives in Rust
/// (<c>libs/fauna-client-search::SearchManager</c>); this view-model snapshots
/// it and raises <see cref="INotifyPropertyChanged"/> for the page to
/// re-render (search.md § State &amp; data shape). The retired ad-hoc VM (its
/// own <c>InitialLimit</c>/<c>LoadMoreStep</c> paging, and a <c>HasMore</c> it
/// *stored* at fetch time instead of deriving) is replaced wholesale — windows
/// carried the same page-ceiling bug every un-migrated leg has
/// (search.md § Implementation status today) until this adoption.
/// <para>Constructor takes the manager and the WinUI-side observer (a
/// <c>SearchNotifyObserver</c> in the app project, passed as the
/// <see cref="SearchSnapshotObserver"/> trait); the VM registers it with the
/// manager and forwards its blanket "everything changed" notification, the
/// same shape <see cref="FeedViewModel"/> uses.</para>
/// <para>Marked <c>internal</c> because the UniFFI-generated types
/// (<see cref="FfiSearchManager"/>, <see cref="SearchSnapshot"/>,
/// <see cref="SearchResultRow"/>) are emitted as <c>internal</c>;
/// <see cref="SearchResult"/> below stays <c>public</c> so the result-card
/// DataTemplate materializes (a WinUI ListView skips rows whose
/// <c>x:DataType</c> is internal).</para>
/// </summary>
internal partial class SearchViewModel : ViewModelBase
{
    private readonly FfiSearchManager _manager;
    // Observer held to keep its callback alive for the manager's lifetime.
    private readonly SearchSnapshotObserver _observer;
    private SearchSnapshot? _cachedSnapshot;

    public SearchViewModel(FfiSearchManager manager, SearchSnapshotObserver observer)
    {
        _manager = manager;
        _observer = observer;

        if (observer is INotifyPropertyChanged inpc)
            inpc.PropertyChanged += (_, _) => { _cachedSnapshot = null; OnPropertyChanged(string.Empty); };

        _manager.AddObserver(observer);
    }

    /// <summary>The shared manager — exposed for the page's direct calls
    /// (run query, load more, cancel); the live query-field buffer stays
    /// plain local page state per search.md § Where logic lives — the page is
    /// submit-driven, no debounce.</summary>
    public FfiSearchManager Manager => _manager;

    public SearchSnapshot Snapshot => _cachedSnapshot ??= _manager.Snapshot();

    // ── Read projections (page rebuilds its bound collection from these) ──

    /// <summary>The last **fired** query — not the live input buffer. Empty ⇒
    /// no search has been run yet (search.md § State &amp; data shape).</summary>
    public string Query => Snapshot.query;

    public string TypeFilter => Snapshot.typeFilter;

    public IReadOnlyList<SearchResult> Results =>
        Snapshot.results.Select(SearchResult.FromRow).ToList();

    public bool IsSearching => Snapshot.inFlight;
    public bool NoResults => Snapshot.noResults;
    public bool HasMore => Snapshot.hasMore;

    /// <summary>Page-level error (`error-message`) resolved from the snapshot's
    /// <c>LocalizedText</c> via the windows i18n pipeline — set when either the
    /// nest or local arm fails, while the other arm's rows stay on screen
    /// (search.md § The local/nest merge).</summary>
    public string? ErrorText => Snapshot.error is { } e ? Strings.Resolve(e) : null;
}

/// <summary>
/// One search hit rendered on the Search page — a display projection of the
/// manager's merged, deduplicated, relevance-ordered <see cref="SearchResultRow"/>.
/// The badge and snippet are already localized/cleaned by the manager (shared
/// Rust); <see cref="Title"/> just runs the badge through the windows i18n
/// table, same as every other <c>LocalizedText</c> on this page.
/// </summary>
public sealed record SearchResult(string Type, string Title, string Snippet, long TimestampMs)
{
    /// <summary>
    /// The typed navigation target (search.md § User actions —
    /// "search-result-item[i] | Open destination"), or <c>null</c> for an
    /// inert row. <c>internal</c> like <see cref="SearchNav"/> itself; the
    /// activation handler lives in the same assembly (the page project),
    /// which shares internal visibility with this one.
    /// </summary>
    internal SearchNav? Nav { get; init; }

    /// Map a merged <c>SearchResultRow</c> to a display row. Internal because
    /// the UniFFI <c>SearchResultRow</c> is internal (CS0051) — the VM (same
    /// assembly) builds the rows; the public record exposes only string/long
    /// props to x:Bind.
    internal static SearchResult FromRow(SearchResultRow r) =>
        new(r.contentType, Strings.Resolve(r.badge), r.snippet, r.timestamp) { Nav = r.navigation };

    /// Relative ("2h ago") timestamp, computed lazily like
    /// <c>FeedPostItem.TimeAgo</c> so it stays fresh across re-renders.
    /// <c>TimestampMs</c> is already epoch millis at the client boundary (the
    /// manager normalizes both backends' native units — search.md § State &amp;
    /// data shape) — no client-side division.
    public string Timestamp => ValueFormat.RelativeTime(
        DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(), TimestampMs);

    public string TypeIcon => Type switch
    {
        "post" => "",     // document
        "imap" => "",     // mail
        "profile" => "",  // contact
        _ => "",          // search
    };
}
