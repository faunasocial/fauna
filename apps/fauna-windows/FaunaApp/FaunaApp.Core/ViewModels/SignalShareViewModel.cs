using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// One <c>signal-share-published-list-item</c> row — a ≥k engagement-signal
/// (or report) aggregate this nest exports to peers (engagement-cues.md §
/// Layer B), projected from an <see cref="FfiReportShareEntry"/>. Read-only;
/// no dispatch. Mirrors <see cref="ReportSharePublishedRow"/> field-for-field
/// (the identical shape — the export view is nest-wide, both
/// <c>signal:*</c> and <c>report:*</c> aggregates).
/// </summary>
public sealed class SignalSharePublishedRow
{
    /// <summary><c>signal-share-published-list-item-hash</c>.</summary>
    public required string ContentHash { get; init; }
    /// <summary><c>signal-share-published-list-item-factor</c>.</summary>
    public required string Factor { get; init; }
    /// <summary><c>signal-share-published-list-item-count</c>.</summary>
    public required string Count { get; init; }

    internal static SignalSharePublishedRow From(FfiReportShareEntry e) => new()
    {
        ContentHash = e.contentHash,
        Factor = e.factor,
        Count = e.count.ToString(),
    };
}

/// <summary>
/// The Personalization home's Layer-B signal-sharing facet VM
/// (engagement-cues.md § Layer B) — the opt-in toggle
/// (<c>personalization-share-signals-toggle</c>) + the read-only
/// transparency pane (<c>signal-share-published-list</c>). A dedicated VM
/// (mirrors <see cref="TrainedTopicsViewModel"/>'s own-VM shape, constructed
/// alongside it in <c>PersonalizationPage.OnNavigatedTo</c>) riding
/// <see cref="INestRpcClient.SignalShareStatusAsync"/> /
/// <see cref="INestRpcClient.SetSignalSharingAsync"/> — which reach the SAME
/// live <c>FfiFeedManager</c> instance the Feed page observes (never a
/// second manager: the manager caches this opt-in for its own signal
/// producer). Mirrors <see cref="MailSpamViewModel"/>'s report-share
/// sub-flow (the identical <see cref="FfiReportShareStatus"/> shape), but as
/// its own VM rather than riding a machine-backed page VM — there is no
/// machine here, just a plain bool + read-only list.
///
/// <para>Deliberately does NOT call <c>HydrateSignalOptinAsync</c> — that FFI
/// face is producer-internal (primes the manager's cached opt-in for a
/// not-yet-built windows capture shell) and no shipped client's pane calls it
/// either (engagement-cues.md § Implementation status today: linux/android/
/// apple's panes all skip it). This lift proves only the toggle round-trip +
/// pane render, exactly as apple's landing did — the producer side (a real
/// dwell-derived verdict) needs a windows capture shell, a separate,
/// not-yet-scoped track.</para>
/// </summary>
public partial class SignalShareViewModel : ViewModelBase
{
    private readonly INestRpcClient _rpc;

    /// <summary><c>personalization-share-signals-toggle</c> state (default
    /// off, user-controls-their-data). Reflects the nest-confirmed opt-in
    /// after each load / toggle — non-optimistic, matching the report-share
    /// precedent (<see cref="MailSpamViewModel.ShareReports"/>).</summary>
    [ObservableProperty] private bool _shareSignals;

    /// <summary>The <c>signal-share-published-list</c> rows — the nest-wide
    /// ≥k transparency export (both <c>signal:*</c> and <c>report:*</c>
    /// aggregates, byte-identical to the federation export), rebuilt on
    /// every load / toggle.</summary>
    public ObservableCollection<SignalSharePublishedRow> PublishedSignals { get; } = new();

    internal SignalShareViewModel(INestRpcClient rpc) => _rpc = rpc;

    /// <summary>Initial page load: read the opt-in + published list
    /// (<c>fauna.moderation.signal_share.status</c>).</summary>
    public async Task LoadAsync()
    {
        ErrorMessage = null;
        try
        {
            Apply(await _rpc.SignalShareStatusAsync());
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Set the opt-in (<c>fauna.moderation.signal_share.set</c>) —
    /// the manager already returns the re-read status in one round trip
    /// (unlike the report-share toggle's bare bool + separate status
    /// re-read), so the toggle + published list reflect the persisted value
    /// directly. Opting out withdraws this actor's contributed signals,
    /// which may shrink the published list.</summary>
    public async Task SetShareSignalsAsync(bool share)
    {
        try
        {
            Apply(await _rpc.SetSignalSharingAsync(share));
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    private void Apply(FfiReportShareStatus status)
    {
        ShareSignals = status.share;
        PublishedSignals.Clear();
        foreach (var e in status.published)
        {
            PublishedSignals.Add(SignalSharePublishedRow.From(e));
        }
    }
}
