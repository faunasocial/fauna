using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Services;
using uniffi.fauna_client_config;

namespace FaunaApp.Core.ViewModels;

/// <summary>One <c>muted-word-item</c> row: the term and its level's token
/// (<c>hide</c> | <c>show-less</c> — <c>fauna_core::scoring::MutedKeywordLevel::value</c>,
/// read through the shared classification, never a weight compared here).</summary>
public sealed record MutedWordRow(string Word, string Level);

/// <summary>
/// The Settings "Muted words" sub-page VM (moderation.md § Muted keywords;
/// content-moderation-and-ranking.md § Q3). A person manages their single
/// user-global tier-1 keyword-mute list here — add a term, see the terms,
/// set a term's level, remove one — over the shared FFI free-fns (each a
/// delta: <c>add_muted_word</c> / <c>remove_muted_word</c> /
/// <c>set_muted_word_level</c>, every one returning the freshly-persisted
/// list, so every mutation here repopulates rather than diffing). Every call
/// hands back the shared page record <c>MutedWordsSnapshot</c> — the terms
/// plus <see cref="Loaded"/>. Unlike the mail-aliases/mail-lists sub-pages
/// there is no per-feature Machine — the shared seam owns the whole round trip
/// (mirrors linux <c>settings/muted_words.rs</c>, priority #2 — no redundant
/// abstraction for a seam this thin).
///
/// Every load/mutation also repopulates <see cref="MutedKeywordsCache"/> — the
/// load-bearing step that keeps the conversation bubble collapse
/// (<c>ConversationsPage.ToMessageView</c>) in sync with every edit, mirroring
/// linux's <c>apply()</c> call to <c>crate::conversations::set_muted_keywords_cache</c>.
/// </summary>
public partial class MutedWordsViewModel : ViewModelBase
{
    private readonly INestRpcClient _nest;

    /// <summary>The rows, each term with its level (settings.md § Muted words —
    /// the level picker): what the page paints.</summary>
    public ObservableCollection<MutedWordRow> Rows { get; } = new();

    /// <summary>The terms alone, in row order.</summary>
    public IReadOnlyList<string> Words => Rows.Select(r => r.Word).ToList();

    /// <summary>
    /// Whether a read has returned successfully — the second painting condition
    /// of <c>muted-word-empty</c> (<c>docs/goal/ui/README.md</c> § <i>List pages:
    /// loading is not empty</i>). <see cref="Rows"/> is empty both before the
    /// first read returns and after one that found nothing, so the page gates its
    /// empty state on this too. Comes from the shared record, never re-derived
    /// here; a failed load leaves it as it was, so a first-read failure keeps the
    /// page unloaded and <c>error-message</c> does the talking.
    /// </summary>
    public bool Loaded { get; private set; }

    /// <summary>Whether a gesture's round trip is in flight — the page disables
    /// the level pickers and the add and remove buttons while it is
    /// (settings.md § Muted words — the level picker). Set before a gesture's
    /// first await, so a caller that starts one and does not yet await it reads
    /// it true.</summary>
    [ObservableProperty] private bool _busy;

    internal MutedWordsViewModel(INestRpcClient nest) => _nest = nest;

    /// <summary>The level picker's options in paint order — the shared catalog
    /// (<c>muted_keyword_levels</c>), each with its token
    /// (<c>muted_keyword_level_value</c>), so this app never spells a level.</summary>
    internal static IReadOnlyList<(string Token, uniffi.fauna_core.MutedKeywordLevel Level)> LevelOptions() =>
        uniffi.fauna_ffi.FaunaFfiMethods.MutedKeywordLevels()
            .Select(level => (uniffi.fauna_ffi.FaunaFfiMethods.MutedKeywordLevelValue(level), level))
            .ToList();

    /// <summary>Hydrate the list from <c>load_muted_words</c>.</summary>
    public async Task LoadAsync()
    {
        ErrorMessage = null;
        try
        {
            Repopulate(await _nest.MutedKeywordsListAsync());
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Start re-reading the list on the shared store-change notice —
    /// call from the page's <c>Page_Loaded</c>, pair with
    /// <see cref="ViewModelBase.CleanupStoreChange"/> on navigate-away.</summary>
    public void WatchStore() => WatchStoreChange(_nest, StoreResyncAsync);

    /// <summary>The store-change re-read (<c>account-runtime.md</c> § A runtime's
    /// own pump is a source of the notice too, part 4): another device's word —
    /// or another device's level for a word — appears on the OPEN page with no
    /// re-visit. Level-safe — it neither clears
    /// <see cref="ViewModelBase.ErrorMessage"/> nor touches the page's add-box
    /// draft (both are the gestures' own), replaces nothing when the read fails,
    /// and answers <c>false</c> (paints nothing) when the list is what shows.</summary>
    internal async Task<bool> StoreResyncAsync()
    {
        var gestures = _gestures;
        var page = await _nest.MutedKeywordsListAsync();
        // A gesture started since the read began has its own, fresher answer.
        if (gestures != _gestures) return false;
        if (Loaded == page.loaded && Rows.SequenceEqual(RowsOf(page)))
            return false;
        Repopulate(page);
        return true;
    }

    /// <summary>Add <paramref name="term"/> (trimmed; a blank term is a no-op,
    /// no RPC call) as a DELTA against the stored list, never this page's copy
    /// wholesale: the shared seam re-reads the list inside its
    /// own CAS update, so a term another device stored since this page loaded
    /// survives this click. A new term is Hide — the add takes no level.</summary>
    public async Task<bool> AddAsync(string term)
    {
        var trimmed = term.Trim();
        if (trimmed.Length == 0) return false;
        return await MutateAsync(() => _nest.MutedKeywordsAddAsync(trimmed));
    }

    /// <summary>Remove <paramref name="term"/> — Add's inverse on the same
    /// delta seam; removing a term another device already deleted is a success
    /// no-op (convergence, not an error).</summary>
    public async Task<bool> RemoveAsync(string term) =>
        await MutateAsync(() => _nest.MutedKeywordsRemoveAsync(term));

    /// <summary>Set <paramref name="term"/>'s level to the one the picker's
    /// value <paramref name="level"/> names (<c>muted_keyword_level_from_value</c>;
    /// a token no level carries sends nothing) — a save at once on the same
    /// delta seam; the row repaints from the record the save returns.</summary>
    public async Task<bool> SetLevelAsync(string term, string level)
    {
        if (uniffi.fauna_ffi.FaunaFfiMethods.MutedKeywordLevelFromValue(level) is not { } picked)
            return false;
        return await MutateAsync(() => _nest.MutedKeywordsSetLevelAsync(term, picked));
    }

    // Bumped as each gesture starts, so an in-flight store-change re-read that a
    // gesture overtook is dropped rather than painted over the gesture's answer.
    private int _gestures;

    private async Task<bool> MutateAsync(Func<Task<MutedWordsSnapshot>> gesture)
    {
        _gestures++;
        ErrorMessage = null;
        Busy = true;
        try
        {
            Repopulate(await gesture());
            return true;
        }
        catch (Exception ex)
        {
            ShowError(ex);
            return false;
        }
        finally
        {
            Busy = false;
        }
    }

    /// <summary>Each stored row's level through the shared classification
    /// (<c>muted_keyword_level</c> → <c>muted_keyword_level_value</c>).</summary>
    private static IEnumerable<MutedWordRow> RowsOf(MutedWordsSnapshot page) =>
        page.keywords.Select(k => new MutedWordRow(
            k.keyword,
            uniffi.fauna_ffi.FaunaFfiMethods.MutedKeywordLevelValue(
                uniffi.fauna_ffi.FaunaFfiMethods.MutedKeywordLevel(k.weight))));

    private void Repopulate(MutedWordsSnapshot page)
    {
        Loaded = page.loaded;
        Rows.Clear();
        foreach (var row in RowsOf(page)) Rows.Add(row);
        MutedKeywordsCache.SetKeywords(page.keywords);
    }
}
