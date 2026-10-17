using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Services;
using uniffi.fauna_conversations;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The permanent Settings → **Members To Review** sub-page
/// (<c>docs/goal/behavior/succession-aftermath.md</c> § Propagation → *Removing
/// a flagged member*). windows was the
/// last of 7 apps still owing this permanent page (the ephemeral kit-side pass
/// is a SEPARATE arm, gated behind windows' identity-stolen ceremony legs —
/// not built here).
///
/// <para><b>There is no sweep gate here, and that is the entire point</b> (tui's
/// own doc on <c>member_review_elements</c>): the ephemeral pass renders only
/// inside a live succession ceremony; everything it leaves unanswered stays
/// open on the succession ledger, and this page is where it lives from then on. The
/// only condition is whether anything is open.</para>
///
/// <para><b>The verdict is DERIVED, never chosen — enforced structurally.</b>
/// <see cref="RemoveAsync"/> calls the ONE FFI door
/// (<c>member_review_remove</c>) that evicts first and persists only what
/// <c>CrossGroupEviction::earned_verdict</c> actually earned; there is no path
/// here that writes <c>Removed</c> from this class's own reasoning. A partial
/// eviction earns none, so the row stays and the page's error banner says how
/// far it got — composed the same way tui's <c>Op::MemberReviewRemove</c> arm
/// composes it (<c>settings.recovery_kit.review_remove_*</c>).</para>
///
/// <para><b><see cref="MemberReviewRow.DisplayText"/> is not a safety verdict</b>
/// and an empty roster is the ordinary state — the page holds only a backlog
/// somebody explicitly postponed (<c>succession-aftermath.md</c> § Implementation
/// status today leaves deliberately no combined "is the user safe" boolean).</para>
/// </summary>
public partial class MemberReviewViewModel : ViewModelBase
{
    private readonly INestRpcClient _rpc;

    // internal, matching every sibling page VM: INestRpcClient is itself internal
    // (the test + WinUI assemblies see it via [InternalsVisibleTo]).
    internal MemberReviewViewModel(INestRpcClient rpc) { _rpc = rpc; }

    [ObservableProperty] private bool _isLoading;

    public ObservableCollection<MemberReviewRow> Reviews { get; } = new();

    /// <summary>Reads the roster on the page's nav edge. <paramref name="manager"/>
    /// resolves each person to the handle they are seated under, the same
    /// <see cref="ConversationsManager"/> instance the caller's session already
    /// holds (never a fresh one) — <c>null</c> renders every row under the "no
    /// longer in any of your groups" wording, same as tui's pre-auth case: an
    /// item nobody can name is still an item nobody can close.</summary>
    internal async Task LoadAsync(ConversationsManager? manager)
    {
        IsLoading = true;
        _opsInFlight++;
        SetError(null);
        try
        {
            await RefreshAsync(manager);
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            _opsInFlight--;
            IsLoading = false;
        }
    }

    private async Task RefreshAsync(ConversationsManager? manager)
    {
        var roster = await _rpc.MemberReviewsListAsync();
        Reviews.Clear();
        foreach (var r in roster)
        {
            var handle = manager?.HandleForPerson(r.person);
            Reviews.Add(MemberReviewRow.From(r, handle));
        }
    }

    /// <summary>Whether two reads of the review roster are the same list — by
    /// content (the generated rows carry byte arrays, which a record compares by
    /// reference). The store-change re-read's "nothing changed" test, shared by every
    /// surface that paints the roster (the page, the member chips, the contact
    /// badges).</summary>
    internal static bool SameRoster(
        IReadOnlyList<FfiMemberReview> a, IReadOnlyList<FfiMemberReview> b)
    {
        if (a.Count != b.Count) return false;
        try
        {
            return System.Text.Json.JsonSerializer.Serialize(a)
                == System.Text.Json.JsonSerializer.Serialize(b);
        }
        catch (Exception)
        {
            // Unserializable: err toward a repaint.
            return false;
        }
    }

    // Loads and verdicts in flight: a store-change re-read waits them out (their
    // own answer repaints the roster).
    private int _opsInFlight;

    /// <summary>Start re-reading the roster on the shared store-change notice —
    /// call from the page's <c>Page_Loaded</c> after <see cref="LoadAsync"/>, pair
    /// with <see cref="ViewModelBase.CleanupStoreChange"/> on navigate-away.
    /// <paramref name="manager"/> resolves handles exactly as the load does (a
    /// getter, so the page's live session manager is read at each notice).</summary>
    internal void WatchStore(Func<ConversationsManager?> manager) =>
        WatchStoreChange(_rpc, () => StoreResyncAsync(manager()));

    /// <summary>The store-change re-read (<c>account-runtime.md</c> § A runtime's
    /// own pump is a source of the notice too, part 4): a person another device
    /// (or the pump's own succession fold) put up for review shows on the OPEN
    /// page with no re-visit. Reads the roster only; skipped while a load or
    /// verdict is in flight; replaces nothing when the read fails or answers what
    /// shows; leaves <see cref="ViewModelBase.ErrorMessage"/> alone — a verdict's
    /// partial-eviction message stays.</summary>
    internal async Task<bool> StoreResyncAsync(ConversationsManager? manager)
    {
        if (_opsInFlight > 0) return false;
        var roster = await _rpc.MemberReviewsListAsync();
        if (_opsInFlight > 0) return false;
        var fresh = roster
            .Select(r => MemberReviewRow.From(r, manager?.HandleForPerson(r.person)))
            .ToList();
        if (fresh.Count == Reviews.Count
            && fresh.Zip(Reviews).All(p =>
                p.First.DisplayText == p.Second.DisplayText
                && p.First.Person.AsSpan().SequenceEqual(p.Second.Person)))
            return false;
        Reviews.Clear();
        foreach (var row in fresh) Reviews.Add(row);
        return true;
    }

    /// <summary>The owner recognises this person — every open item for them
    /// closes with no group changes. The identical write the ephemeral pass
    /// would make, so the two surfaces cannot drift on what Keep means.
    /// </summary>
    internal async Task KeepAsync(byte[] person, ConversationsManager? manager)
    {
        SetError(null);
        _opsInFlight++;
        try
        {
            await _rpc.MemberReviewKeepAsync(person);
            await RefreshAsync(manager);
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            _opsInFlight--;
        }
    }

    /// <summary>Evict <paramref name="person"/> from every group they are in
    /// now and record whatever verdict was earned. A complete eviction (or one
    /// that frees nobody — the ordinary case on a deferred backlog) re-reads
    /// the roster so the row clears; a partial one leaves the roster alone and
    /// surfaces why, composed the same way tui's <c>Op::MemberReviewRemove</c>
    /// arm does.</summary>
    internal async Task RemoveAsync(byte[] person, ConversationsManager manager)
    {
        SetError(null);
        _opsInFlight++;
        try
        {
            var eviction = await _rpc.MemberReviewRemoveAsync(manager, person);
            if (eviction.@failed.Length == 0 && eviction.@unreachable.Length == 0)
            {
                // Complete -- Removed was earned and already persisted by the
                // FFI call itself; re-read so the row clears.
                await RefreshAsync(manager);
            }
            else
            {
                // Partial -- no verdict was earned, the row stays.
                var who = manager.HandleForPerson(person)
                          ?? Strings.Get("settings/recovery_kit/review_unknown_person");
                SetError(ComposeRemovePartialMessage(who, eviction));
            }
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            _opsInFlight--;
        }
    }

    /// <summary>Mirrors tui's <c>Op::MemberReviewRemove</c> arm's message
    /// composition exactly — one lead line for what happened here, then one
    /// sentence per rule-(5) blocking class naming the room its remedy lives
    /// in (<c>succession-aftermath.md</c> § Propagation, rule 5). A blocked
    /// verdict with no named remedy would read as a button that silently
    /// stopped working.</summary>
    internal static string ComposeRemovePartialMessage(string who, CrossGroupEviction eviction)
    {
        var parts = new List<string>();
        var groups = eviction.@evicted.Length + eviction.@failed.Length;
        if (eviction.@failed.Length > 0)
        {
            parts.Add(Strings.Get("settings/recovery_kit/review_remove_partial")
                .Replace("{who}", who)
                .Replace("{removed}", eviction.@evicted.Length.ToString())
                .Replace("{groups}", groups.ToString()));
        }
        else if (eviction.@evicted.Length > 0)
        {
            parts.Add(Strings.Get("settings/recovery_kit/review_remove_done_here")
                .Replace("{who}", who)
                .Replace("{removed}", eviction.@evicted.Length.ToString()));
        }
        else
        {
            parts.Add(Strings.Get("settings/recovery_kit/review_remove_none_here")
                .Replace("{who}", who));
        }

        var folders = eviction.@unreachable.Count(s => s.@class == UnreachableSeatClass.FolderChannel);
        if (folders > 0)
        {
            parts.Add(Strings.Get("settings/recovery_kit/review_remove_folder_seats")
                .Replace("{seats}", folders.ToString()));
        }
        var unsynced = eviction.@unreachable.Count(s => s.@class == UnreachableSeatClass.ChatGroupNoThreadHere);
        if (unsynced > 0)
        {
            parts.Add(Strings.Get("settings/recovery_kit/review_remove_unsynced_seats")
                .Replace("{seats}", unsynced.ToString()));
        }
        return string.Join(" ", parts);
    }
}

/// <summary>One <c>member-review-row</c> — the display text already fully
/// composed, so the page stays a pure renderer with no formatting logic of
/// its own. <see cref="Person"/> is the raw 32-byte actor id, carried for the
/// Keep/Remove buttons: address by the PERSON, never the painted index — the
/// roster re-orders on every re-read.</summary>
public record MemberReviewRow(byte[] Person, string DisplayText)
{
    internal static MemberReviewRow From(FfiMemberReview review, string? handle)
    {
        var parts = FaunaFfiMethods.MemberReviewRowText(review.@person, review.@reasons, handle);
        var who = Strings.Resolve(parts.@who);
        var reason = string.Join(", ", parts.@reasons.Select(Strings.Resolve));
        var text = Strings.Get("settings/recovery_kit/review_row")
            .Replace("{who}", who)
            .Replace("{reason}", reason);
        return new MemberReviewRow(review.@person, text);
    }
}
