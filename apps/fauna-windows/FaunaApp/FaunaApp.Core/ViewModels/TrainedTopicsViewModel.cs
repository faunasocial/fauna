using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Personalization home's Trained-topics facet VM (topic-factors.md
/// § Authoring surface &amp; picker). List/create/rename/delete over the shared
/// FFI free-fns <c>trained_topics_{list,create,rename,delete}</c> — thin
/// pass-throughs of <c>fauna_client_personalization::topics::TrainedTopics</c>,
/// which owns the registry↔model-plane sequencing (the advisory example-count
/// read, the create cap, the delete's registry-removal-then-model-delete
/// pairing) — no client-side re-derivation of that logic (priority #2).
/// Mirrors linux <c>views/personalization/trained_topics.rs</c> (the reference
/// leg) and <see cref="MutedWordsViewModel"/>'s load/mutate/repopulate shape.
/// </summary>
public partial class TrainedTopicsViewModel : ViewModelBase
{
    private readonly INestRpcClient _nest;

    // Monotonic dispatch counter, bumped at the START of every RunAsync-wrapped
    // call (the nav-edge/login-time LoadAsync included) and captured into a
    // local before the await. `async void` callers (PersonalizationPage's
    // Page_Loaded and its gesture click handlers) fire independent, unserialized
    // RunAsync calls, so without this a slower LoadAsync can resolve AFTER a
    // faster create/rename/delete/toggle and clobber the fresher rows with
    // stale ones.
    // C# async/await on the single-threaded UI dispatcher means the ordering
    // hazard is purely "whichever RunAsync's continuation runs last wins", the
    // same shape as tui's tokio::spawn race; no cross-thread sync is needed
    // beyond the sequence comparison itself.
    private long _dispatchSeq;

    // The `_dispatchSeq` of the last outcome (success OR failure) accepted —
    // mirrors tui's `applied_seq` (apps/fauna-tui/src/settings/trained_topics.rs).
    // A result whose captured seq is older is dropped as a silent no-op, never
    // an error: the newer op it lost the race to already landed the true state.
    private long _appliedSeq;

    // internal: FfiTrainedTopicRow is a UniFFI-internal generated record (a public
    // member can't expose a less-accessible type, CS0053) — mirrors
    // TaskDelegationViewModel.Rows / TaskDelegationRowVm. The consuming page lives
    // in the FaunaApp assembly, which sees FaunaApp.Core's internals.
    internal ObservableCollection<FfiTrainedTopicRow> Topics { get; } = new();

    internal TrainedTopicsViewModel(INestRpcClient nest) => _nest = nest;

    /// <summary>Hydrate the facet from <c>list_trained_topics</c>.</summary>
    public async Task LoadAsync() => await RunAsync(() => _nest.TrainedTopicsListAsync());

    /// <summary>Mint a factor (trimmed; a blank name is a client-side no-op —
    /// the shared service's <c>BlankName</c> rejection is defense in depth, never
    /// reachable through this guard, mirroring linux's <c>submit()</c>).</summary>
    public async Task<bool> CreateAsync(string name)
    {
        var trimmed = name.Trim();
        if (trimmed.Length == 0) return false;
        return await RunAsync(() => _nest.TrainedTopicsCreateAsync(trimmed));
    }

    /// <summary>Rename in place — the id (and so the derived composition key)
    /// is untouched.</summary>
    public async Task<bool> RenameAsync(byte[] id, string name)
    {
        var trimmed = name.Trim();
        if (trimmed.Length == 0) return false;
        return await RunAsync(() => _nest.TrainedTopicsRenameAsync(id, trimmed));
    }

    /// <summary>Remove the registry entry AND its paired nest-side model row.</summary>
    public async Task<bool> DeleteAsync(byte[] id) => await RunAsync(() => _nest.TrainedTopicsDeleteAsync(id));

    /// <summary>Flip the row's Layer-A opt-in ("Learn from my activity";
    /// engagement-cues.md § Layer A). Registry-only — the model row is
    /// untouched, so turning it off stops future weak training without
    /// rewriting what engagement already taught.</summary>
    public async Task<bool> SetLearnFromEngagementAsync(byte[] id, bool on) =>
        await RunAsync(() => _nest.TrainedTopicsSetLearnFromEngagementAsync(id, on));

    // ── Publishing a trained factor as a List (topic-factors.md § Publishing
    // a trained factor; frame D8) — bundled into this facet's own VM, not a
    // second view-model (mirrors android's TrainedTopicsVM bundling the same
    // sheet state, and linux's single Ctx / web's single-component shape). ──

    /// <summary>Score the loaded feed window with one trained factor for the
    /// publish review-prune sheet — a thin pass-through to the SAME live
    /// FeedManager the Feed page observes (<see cref="INestRpcClient.ScoreCorpusForFactorAsync"/>),
    /// never a freshly-built one (which would have no loaded window to
    /// score). Errors propagate as exceptions rather than routing through
    /// <see cref="ErrorMessage"/>: this can be the FIRST call the sheet makes,
    /// and must not blank out the Topics list's own error surface for an
    /// unrelated facet — the sheet owns its own error display.</summary>
    internal async Task<ScoredExemplar[]> ScoreCorpusForFactorAsync(string factor) =>
        await _nest.ScoreCorpusForFactorAsync(factor);

    /// <summary>Publish the review sheet's pruned exemplar set as a tier-3
    /// List labeler (<c>trained_topic_publish_list</c>) — owns the entire
    /// lifecycle (derive the per-factor keypair, resolve the next version,
    /// build + sign, <c>fauna.labelers.publish</c>). Propagates
    /// <see cref="FfiPublishListException"/> or a transport exception; the
    /// sheet's own submit handles both (see <see cref="LocalizePublish"/>),
    /// exactly like <see cref="ScoreCorpusForFactorAsync"/> above.</summary>
    internal async Task<FfiPublishedList> TrainedTopicPublishListAsync(
        byte[] factorId, string name, IReadOnlyList<FfiPublishEntry> entries) =>
        await _nest.TrainedTopicPublishListAsync(factorId, name, entries);

    /// <summary>The one place the shared crate's typed publish errors become
    /// user-facing text (topic-factors.md § Publishing a trained factor). No
    /// dedicated i18n copy is ratified for <c>BlankName</c>/<c>NameTooLong</c>
    /// yet — mirrors apple's <c>mapPublishError</c> and android's raw
    /// <c>e.message</c> fallback (both already-shipped legs settled on the
    /// boundary's own message rather than inventing new copy; priority #1 —
    /// matching the richest already-established pattern across clients over
    /// re-deriving a windows-only phrasing keeps every app behaving
    /// identically here). Only <c>General</c> carries a clean human sentence
    /// the crate already built; the other two variants fall back to the
    /// exception's own message.</summary>
    internal static string LocalizePublish(FfiPublishListException ex) => ex switch
    {
        FfiPublishListException.General g => g.msg,
        _ => ex.Message,
    };

    // ── Publishing a trained factor as a Model (topic-factors.md § Publishing
    // a trained factor, v2) — the List section's exact shape. ─────────────────

    /// <summary>Rebuild the factor's publishable vocabulary for the publish
    /// review-prune sheet's Model kind — a thin pass-through to the SAME live
    /// FeedManager the Feed page observes (<see cref="INestRpcClient.ScrubCorpusForFactorAsync"/>).
    /// Unlike <see cref="ScoreCorpusForFactorAsync"/> there is no top-N: the
    /// vocabulary IS the disclosure. Errors propagate as exceptions, exactly
    /// like the List twin above.</summary>
    internal async Task<TrainedModelReview> ScrubCorpusForFactorAsync(string factor) =>
        await _nest.ScrubCorpusForFactorAsync(factor);

    /// <summary>Publish the review sheet's pruned n-gram set as a tier-3 Model
    /// labeler (<c>trained_topic_publish_model</c>). <paramref
    /// name="moreDocs"/>/<paramref name="lessDocs"/> are passed UNSHRUNK — the
    /// corpus's own counters, not the pruned entry count. Propagates
    /// <see cref="FfiPublishModelException"/> or a transport exception; the
    /// sheet's own submit handles both (see <see cref="LocalizePublishModel"/>).</summary>
    internal async Task<FfiPublishedModel> TrainedTopicPublishModelAsync(
        byte[] factorId, string name, uint moreDocs, uint lessDocs, IReadOnlyList<FfiPublishNgram> ngrams) =>
        await _nest.TrainedTopicPublishModelAsync(factorId, name, moreDocs, lessDocs, ngrams);

    /// <summary><see cref="LocalizePublish"/>'s shape, over the Model kind's
    /// error variants. <c>EmptyVocabulary</c> is the one refusal a user fixes
    /// by marking more public posts rather than by editing the sheet (§
    /// Publishing) — no dedicated copy is ratified for it yet either, so it
    /// falls back to the raw error like the other two variants (mirrors
    /// apple's <c>mapPublishModelError</c>).</summary>
    internal static string LocalizePublishModel(FfiPublishModelException ex) => ex switch
    {
        FfiPublishModelException.General g => g.msg,
        _ => ex.Message,
    };

    private async Task<bool> RunAsync(Func<Task<FfiTrainedTopicRow[]>> op)
    {
        var capturedSeq = ++_dispatchSeq;
        ErrorMessage = null;
        try
        {
            var rows = await op();
            // A stale success still means the nest-side op succeeded — a
            // fresher op's result already landed, so skip Repopulate (it would
            // reintroduce/discard rows) but still report success: the caller
            // (e.g. SubmitTrainedTopicAsync) uses the return value to clear its
            // input, and that op DID commit server-side.
            if (capturedSeq < _appliedSeq) return true;
            _appliedSeq = capturedSeq;
            Repopulate(rows);
            return true;
        }
        catch (FfiTrainedTopicsException ex)
        {
            if (capturedSeq < _appliedSeq) return false;
            _appliedSeq = capturedSeq;
            SetError(Localize(ex));
            return false;
        }
        catch (Exception ex)
        {
            if (capturedSeq < _appliedSeq) return false;
            _appliedSeq = capturedSeq;
            ShowError(ex);
            return false;
        }
    }

    private void Repopulate(FfiTrainedTopicRow[] rows)
    {
        Topics.Clear();
        foreach (var row in rows) Topics.Add(row);
    }

    /// <summary>The one place the FFI's structured error becomes user-facing
    /// text — the cap message names the limit, so it needs the number rather
    /// than the generator's raw exception <c>Message</c> (a "@field=value"
    /// debug string, never shown to a user).</summary>
    private static string Localize(FfiTrainedTopicsException ex) => ex switch
    {
        FfiTrainedTopicsException.Cap c => Strings.Format("personalization/trained_factor_cap", c.max.ToString()),
        FfiTrainedTopicsException.General g => g.msg,
        // BlankName is unreachable through CreateAsync/RenameAsync's own trim-guard
        // above (the client-side twin of linux's submit() early-return); kept only
        // as a defensive fallback for the shared service's own validation.
        _ => "a trained topic needs a name",
    };
}
