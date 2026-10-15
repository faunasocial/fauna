using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Helpers;
using FaunaApp.Services;
using FaunaApp.Sync;
using uniffi.fauna_labeler_catalog_machine;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Community labelers sub-page (content-moderation-and-ranking.md §
/// Tier-3 community models &amp; background re-processing; ui.yaml
/// labeler-catalog): browse + inspect-before-subscribe over EVERY published
/// labeler. A thin renderer over its OWN <see cref="LabelerCatalogMachine"/>
/// instance (the windows per-page-machine convention — mirrors
/// <see cref="PersonalizationPage"/>/<see cref="DevicesPage"/>).
///
/// Mirrors apps/fauna-linux/src/views/personalization/mod.rs's CatalogShell
/// (the lift reference this page tracks 1:1): every entry renders with inspect
/// + subscribe (hidden once subscribed) + unsubscribe (shown once subscribed);
/// inspecting decodes + client-side re-verifies the signed metadata
/// (<c>LabelerInspectView.verified</c> — the tier-3 transparency requirement
/// holding even against a compromised nest).
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> — a WinUI page that mutates bound
/// state off the UI thread throws a silent <c>COMException</c>
/// (reference_windows_vm_configureawait_comexception).</para>
/// </summary>
public sealed partial class LabelerCatalogPage : Page
{
    private INestRpcClient? _rpc;

    private LabelerCatalogMachine? _machine;
    private LabelerCatalogNotifyObserver? _observer;

    public LabelerCatalogPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _rpc = clients.Rpc;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_rpc is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            // Build the page machine bound to the session WS-RPC requester; the
            // observer ticks the UI thread (LabelerCatalogNotifyObserver) → RenderPage.
            _observer = new LabelerCatalogNotifyObserver(RenderPage);
            _machine = await _rpc.BuildLabelerCatalogMachineAsync(_observer);
            await _machine.Refresh();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
        LoadProgress.IsActive = false;
        LoadProgress.Visibility = Visibility.Collapsed;
        RenderPage();
    }

    private void RenderPage()
    {
        if (_machine is null) return;
        var snap = _machine.Snapshot();

        // Every published labeler — inspect + subscribe (hidden once subscribed)
        // + unsubscribe (shown once subscribed), all index-addressed into this
        // same unfiltered entries array. Subscribe paints from the machine's one
        // read of the inspect gate (SubscribeEnabled), repainted on every tick.
        CatalogContainer.Children.Clear();
        var entries = snap.entries;
        for (var i = 0; i < entries.Length; i++)
        {
            var index = (uint)i;
            var row = LabelerCatalogRowBuilder.BuildRow(
                entries[i],
                index,
                showInspectSubscribe: true,
                subscribeEnabled: _machine.SubscribeEnabled(index),
                onInspect: idx => _machine!.Inspect(idx),
                onSubscribe: idx => _machine!.Subscribe(idx),
                onUnsubscribe: idx => _machine!.Unsubscribe(idx));
            CatalogContainer.Children.Add(row);
        }
        // Loaded AND empty — gating on `loaded` stops the empty state from painting
        // over a read still in flight (docs/goal/ui/README.md § List pages: loading
        // is not empty).
        CatalogEmptyText.Visibility = snap.loaded && entries.Length == 0
            ? Visibility.Visible
            : Visibility.Collapsed;

        InspectPanel.Render(snap.inspecting);

        // Page-level error-message: the machine localizes the last read/gesture
        // failure into snapshot.error; a subsequent successful gesture clears it.
        var err = snap.error;
        if (err is not null)
        {
            var msg = S.Resolve(err);
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
        else
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
    }

    // labeler-inspect-close-button: close_inspect() clears
    // LabelerCatalogSnapshot.inspecting and ticks the observer itself
    // (libs/fauna-labeler-catalog-machine/src/machine.rs) — no manual re-render
    // needed here.
    private void InspectPanel_CloseRequested(object? sender, EventArgs e)
        => _machine?.CloseInspect();
}
