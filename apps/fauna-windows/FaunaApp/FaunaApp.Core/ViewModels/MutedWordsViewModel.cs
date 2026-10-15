using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_client_config;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Settings "Muted words" sub-page VM (moderation.md § Muted keywords;
/// content-moderation-and-ranking.md § Q3). A person manages their single
/// user-global tier-1 keyword-mute list here — add a term, see the terms,
/// remove one — over the shared FFI free-fns <c>muted_keywords_{list,set}</c>
/// (backups-destinations shaped: <c>save_muted_words</c> replaces the whole
/// list and returns the normalized, freshly-persisted one, so every mutation
/// here loads-then-repopulates rather than diffing). Both calls hand back the
/// shared page record <c>MutedWordsSnapshot</c> — the terms plus <see
/// cref="Loaded"/>. Unlike the mail-aliases/mail-lists sub-pages there is no
/// per-feature Machine — the shared seam owns the whole round trip (mirrors
/// linux <c>settings/muted_words.rs</c>, priority #2 — no redundant abstraction
/// for a seam this thin).
///
/// Every load/mutation also repopulates <see cref="MutedKeywordsCache"/> — the
/// load-bearing step that keeps the conversation bubble collapse
/// (<c>ConversationsPage.ToMessageView</c>) in sync with every edit, mirroring
/// linux's <c>apply()</c> call to <c>crate::conversations::set_muted_keywords_cache</c>.
/// </summary>
public partial class MutedWordsViewModel : ViewModelBase
{
    private readonly INestRpcClient _nest;

    public ObservableCollection<string> Words { get; } = new();

    /// <summary>
    /// Whether a read has returned successfully — the second painting condition
    /// of <c>muted-word-empty</c> (<c>docs/goal/ui/README.md</c> § <i>List pages:
    /// loading is not empty</i>). <see cref="Words"/> is empty both before the
    /// first read returns and after one that found nothing, so the page gates its
    /// empty state on this too. Comes from the shared record, never re-derived
    /// here; a failed load leaves it as it was, so a first-read failure keeps the
    /// page unloaded and <c>error-message</c> does the talking.
    /// </summary>
    public bool Loaded { get; private set; }

    internal MutedWordsViewModel(INestRpcClient nest) => _nest = nest;

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
    /// own pump is a source of the notice too, part 4): another device's word
    /// appears on the OPEN page with no re-visit. Level-safe — it neither clears
    /// <see cref="ViewModelBase.ErrorMessage"/> nor touches the page's add-box
    /// draft (both are the gestures' own), replaces nothing when the read fails,
    /// and answers <c>false</c> (paints nothing) when the list is what shows.</summary>
    internal async Task<bool> StoreResyncAsync()
    {
        var gestures = _gestures;
        var page = await _nest.MutedKeywordsListAsync();
        // A gesture started since the read began has its own, fresher answer.
        if (gestures != _gestures) return false;
        if (Loaded == page.loaded && Words.SequenceEqual(page.keywords.Select(k => k.keyword)))
            return false;
        Repopulate(page);
        return true;
    }

    /// <summary>Add <paramref name="term"/> (trimmed; a blank term is a no-op,
    /// no RPC call) as a DELTA against the stored list, never this page's copy
    /// wholesale: the shared seam re-reads the list inside its
    /// own CAS update, so a term another device stored since this page loaded
    /// survives this click.</summary>
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

    // Bumped as each gesture starts, so an in-flight store-change re-read that a
    // gesture overtook is dropped rather than painted over the gesture's answer.
    private int _gestures;

    private async Task<bool> MutateAsync(Func<Task<MutedWordsSnapshot>> gesture)
    {
        _gestures++;
        ErrorMessage = null;
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
    }

    private void Repopulate(MutedWordsSnapshot page)
    {
        Loaded = page.loaded;
        Words.Clear();
        foreach (var k in page.keywords) Words.Add(k.keyword);
        MutedKeywordsCache.SetKeywords(page.keywords);
    }
}
