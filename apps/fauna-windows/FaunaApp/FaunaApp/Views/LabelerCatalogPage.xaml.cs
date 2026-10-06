using System;
using System.Text;
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
        // same unfiltered entries array.
        CatalogContainer.Children.Clear();
        var entries = snap.entries;
        for (var i = 0; i < entries.Length; i++)
        {
            var index = (uint)i;
            var row = LabelerCatalogRowBuilder.BuildRow(
                entries[i],
                index,
                showInspectSubscribe: true,
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

        RenderInspectPanel(snap.inspecting);

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

    private void RenderInspectPanel(LabelerInspectView? inspecting)
    {
        if (inspecting is null)
        {
            InspectPanelBorder.Visibility = Visibility.Collapsed;
            return;
        }
        // Bool fields print lowercase "true"/"false" (Rust's Display convention,
        // and what every other app's metadata dump already emits) — C#'s
        // default bool.ToString() capitalizes ("True"/"False"), which is a
        // pre-existing formatting bug this List-kind leg is the first test to
        // actually check (test_trained_topics.py's "verified: true" assertion).
        var sb = new StringBuilder();
        sb.AppendLine($"labeler_id: {inspecting.labelerId}");
        sb.AppendLine($"artifact_kind: {inspecting.artifactKind}");
        sb.AppendLine($"version: {inspecting.version}");
        sb.AppendLine($"wasm_hash: {inspecting.wasmHash}");
        sb.AppendLine($"wasm_size: {inspecting.wasmSize}");
        sb.AppendLine($"needs_text: {Lower(inspecting.needsText)}");
        sb.AppendLine($"needs_hashtags: {Lower(inspecting.needsHashtags)}");
        sb.AppendLine($"needs_media_metadata: {Lower(inspecting.needsMediaMetadata)}");
        sb.AppendLine($"needs_author: {Lower(inspecting.needsAuthor)}");
        sb.AppendLine($"needs_attachment_bytes: {Lower(inspecting.needsAttachmentBytes)}");
        sb.Append($"verified: {Lower(inspecting.verified)}");
        InspectMetadataText.Text = sb.ToString();

        // list-kind only (content-moderation-and-ranking.md § Tier-3 artifact
        // kinds; topic-factors.md § Publishing): the decoded publisher-chosen
        // name + the EXACT id→score map, every entry — never a capped preview.
        // The frame's "inspect returns the raw artifact, so the client renders
        // the exact id→score map" is met by the shared LabelerCatalogMachine's
        // own client-side decode/verify (validate_list_artifact); this page
        // only renders what's already in the snapshot.
        var isList = inspecting.artifactKind == "list";
        InspectListSection.Visibility = isList ? Visibility.Visible : Visibility.Collapsed;
        InspectListEntriesContainer.Children.Clear();
        if (isList)
        {
            InspectListNameText.Text = string.IsNullOrEmpty(inspecting.listName)
                ? S.Get("labeler_catalog/unnamed_list")
                : S.Format("labeler_catalog/list_name", inspecting.listName);
            InspectListEntryCountText.Text = S.Format("labeler_catalog/list_entry_count", inspecting.listEntries.Length.ToString());
            foreach (var entry in inspecting.listEntries)
            {
                InspectListEntriesContainer.Children.Add(BuildInspectListEntryRow(entry));
            }
        }

        // text-model-kind only, the SAME optional-elements shape as list
        // above: registers only for a text-model artifact, keyed on the KIND
        // rather than payload non-emptiness (an empty vocabulary is a decode
        // failure, not "some other kind"). Mirrors tui's push_inspect_panel
        // Model branch (content-moderation-and-ranking.md § Tier-3 artifact
        // kinds: inspect renders the full vocabulary — EVERY entry, never a
        // capped preview).
        var isModel = inspecting.artifactKind == "text-model";
        InspectModelSection.Visibility = isModel ? Visibility.Visible : Visibility.Collapsed;
        InspectModelEntriesContainer.Children.Clear();
        if (isModel)
        {
            InspectModelNameText.Text = string.IsNullOrEmpty(inspecting.modelName)
                ? S.Get("labeler_catalog/unnamed_model")
                : S.Format("labeler_catalog/model_name", inspecting.modelName);
            InspectModelNgramCountText.Text = S.Format("labeler_catalog/model_ngram_count", inspecting.modelNgrams.Length.ToString());
            foreach (var entry in inspecting.modelNgrams)
            {
                InspectModelEntriesContainer.Children.Add(BuildInspectModelEntryRow(entry));
            }
        }

        InspectPanelBorder.Visibility = Visibility.Visible;
    }

    private static string Lower(bool b) => b ? "true" : "false";

    /// <summary>One <c>labeler-inspect-list-entry</c> row: the exact
    /// content-id + per-mille score, verbatim from the artifact (no rescale).</summary>
    private static FrameworkElement BuildInspectListEntryRow(LabelerInspectListEntry entry)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.LabelerInspectListEntry);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, entry.contentId);

        var id = new TextBlock
        {
            Text = entry.contentId,
            IsTextSelectionEnabled = true,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(id, Ids.LabelerInspectListEntryId);
        row.Children.Add(id);

        var score = new TextBlock { Text = entry.score.ToString(), Opacity = 0.7 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(score, Ids.LabelerInspectListEntryScore);
        row.Children.Add(score);

        return row;
    }

    /// <summary>One <c>labeler-inspect-model-entry</c> row: the n-gram itself
    /// + its class direction + distinct-document count — the SAME two shared
    /// faces (<see cref="ValueFormat.NgramDirectionLabel"/>/
    /// <see cref="ValueFormat.NgramDocCountLabel"/>) the publisher's own
    /// review rows painted, so what a publisher was shown before publishing
    /// is exactly what a subscriber reads before subscribing.</summary>
    private static FrameworkElement BuildInspectModelEntryRow(LabelerInspectModelNgram entry)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.LabelerInspectModelEntry);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, entry.ngram);

        var text = new TextBlock
        {
            Text = entry.ngram,
            IsTextSelectionEnabled = true,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(text, Ids.LabelerInspectModelEntryText);
        row.Children.Add(text);

        var direction = new TextBlock { Text = ValueFormat.NgramDirectionLabel(entry.more, entry.less), Opacity = 0.7 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(direction, Ids.LabelerInspectModelEntryDirection);
        row.Children.Add(direction);

        var count = new TextBlock { Text = ValueFormat.NgramDocCountLabel(entry.more, entry.less), Opacity = 0.7 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(count, Ids.LabelerInspectModelEntryCount);
        row.Children.Add(count);

        return row;
    }

    // labeler-inspect-close-button: close_inspect() clears
    // LabelerCatalogSnapshot.inspecting and ticks the observer itself
    // (libs/fauna-labeler-catalog-machine/src/machine.rs) — no manual re-render
    // needed here.
    private void InspectCloseButton_Click(object sender, RoutedEventArgs e)
        => _machine?.CloseInspect();
}
