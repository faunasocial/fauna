using System;
using System.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Services;
using uniffi.fauna_labeler_catalog_machine;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Controls;

/// <summary>
/// The labeler catalog's inspect view (<c>labeler-inspect-panel</c>): the signed
/// metadata a labeler publishes, re-verified client-side, plus the exact id→score
/// map of a list artifact or the whole vocabulary of a text-model one
/// (content-moderation-and-ranking.md § Tier-3 artifact kinds).
///
/// <para>A thin renderer over <see cref="LabelerInspectView"/>, lifted out of
/// <c>LabelerCatalogPage</c> so the Community labelers page and the room editor's
/// labeler set paint the SAME view (<c>ui/conversations.md</c> § Element IDs →
/// <i>The room's labeler set</i>: "the catalog's own inspect view, in place").
/// What the panel closes onto is the host's: <see cref="CloseRequested"/> fires on
/// <c>labeler-inspect-close-button</c> and the host calls the machine's
/// <c>CloseInspect</c>.</para>
/// </summary>
public sealed partial class LabelerInspectPanel : UserControl
{
    /// <summary><c>labeler-inspect-close-button</c> was pressed.</summary>
    public event EventHandler? CloseRequested;

    public LabelerInspectPanel() { InitializeComponent(); }

    /// <summary>Paint <paramref name="inspecting"/>; <c>null</c> collapses the panel
    /// (absent to a driver).</summary>
    internal void Render(LabelerInspectView? inspecting)
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
        sb.AppendLine($"label_abi: {inspecting.labelAbi}");
        sb.Append($"verified: {Lower(inspecting.verified)}");
        // A revision newer than this app's own reads inert and says so, in the
        // same localized sentence the refused subscribe puts on error-message.
        if (inspecting.newerThanThisApp)
        {
            sb.Append('\n').Append(S.Get("labeler_catalog/inspect_needs_newer_app"));
        }
        InspectMetadataText.Text = sb.ToString();

        // list-kind only (content-moderation-and-ranking.md § Tier-3 artifact
        // kinds; topic-factors.md § Publishing): the decoded publisher-chosen
        // name + the EXACT id→score map, every entry — never a capped preview.
        // The frame's "inspect returns the raw artifact, so the client renders
        // the exact id→score map" is met by the shared LabelerCatalogMachine's
        // own client-side decode/verify (validate_list_artifact); this panel
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
        AutomationProperties.SetAutomationId(row, Ids.LabelerInspectListEntry);
        AutomationProperties.SetName(row, entry.contentId);

        var id = new TextBlock
        {
            Text = entry.contentId,
            IsTextSelectionEnabled = true,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        AutomationProperties.SetAutomationId(id, Ids.LabelerInspectListEntryId);
        row.Children.Add(id);

        var score = new TextBlock { Text = entry.score.ToString(), Opacity = 0.7 };
        AutomationProperties.SetAutomationId(score, Ids.LabelerInspectListEntryScore);
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
        AutomationProperties.SetAutomationId(row, Ids.LabelerInspectModelEntry);
        AutomationProperties.SetName(row, entry.ngram);

        var text = new TextBlock
        {
            Text = entry.ngram,
            IsTextSelectionEnabled = true,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        AutomationProperties.SetAutomationId(text, Ids.LabelerInspectModelEntryText);
        row.Children.Add(text);

        var direction = new TextBlock { Text = ValueFormat.NgramDirectionLabel(entry.more, entry.less), Opacity = 0.7 };
        AutomationProperties.SetAutomationId(direction, Ids.LabelerInspectModelEntryDirection);
        row.Children.Add(direction);

        var count = new TextBlock { Text = ValueFormat.NgramDocCountLabel(entry.more, entry.less), Opacity = 0.7 };
        AutomationProperties.SetAutomationId(count, Ids.LabelerInspectModelEntryCount);
        row.Children.Add(count);

        return row;
    }

    private void InspectCloseButton_Click(object sender, RoutedEventArgs e)
        => CloseRequested?.Invoke(this, EventArgs.Empty);
}
