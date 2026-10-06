using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Services;
using uniffi.fauna_labeler_catalog_machine;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Helpers;

/// <summary>
/// Builds one <c>labeler-catalog-item</c> row (ui.yaml component, shared by the
/// Personalization home's subscribed-labelers facet and the Community-labelers
/// catalog page — content-moderation-and-ranking.md § Tier-3 community models).
/// Mirrors linux's <c>build_labeler_row</c>
/// (apps/fauna-linux/src/views/personalization/mod.rs): <paramref name="index"/>
/// is the entry's ORIGINAL index into the machine's full, unfiltered
/// <c>LabelerCatalogSnapshot.entries</c> — a caller that filters the list (the
/// personalization home's subscribed-only view) must preserve it, since
/// Inspect/Subscribe/Unsubscribe are index-addressed into the full array.
/// <paramref name="showInspectSubscribe"/> gates the inspect + subscribe
/// affordances (labeler-catalog page only, per ui.yaml's <c>labeler-catalog-item</c>
/// description) — unsubscribe is always present so the personalization home can
/// un-subscribe inline.
/// </summary>
internal static class LabelerCatalogRowBuilder
{
    public static FrameworkElement BuildRow(
        LabelerCatalogEntry entry,
        uint index,
        bool showInspectSubscribe,
        Func<uint, Task> onInspect,
        Func<uint, Task> onSubscribe,
        Func<uint, Task> onUnsubscribe)
    {
        // Grid (star info column + Auto button columns), NOT a horizontal
        // StackPanel: an unconstrained StackPanel row (info + up to 2 buttons)
        // can grow past the settings-content column's visible width, pushing
        // the trailing button off-screen (FlaUI IsOffscreen — is_visible false
        // even though the row itself renders; reference_windows_e2e_is_visible_
        // offscreen's horizontal analogue). The star column absorbs/shrinks to
        // the available width instead, keeping the Auto-sized buttons always
        // in view — mirrors DevicesPage's device-card Grid (Auto, *, Auto, Auto).
        var row = new Grid { ColumnSpacing = 12, Padding = new Thickness(8) };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        AutomationProperties.SetAutomationId(row, Ids.LabelerCatalogItem);
        // AutomationProperties.Name keeps the row in the UIA content view so
        // FlaUI's ByAutomationId resolves the nested child ids
        // (reference_winui_flaui_datatemplate_name); the factor is unique per
        // labeler and already rendered as a visible field below.
        AutomationProperties.SetName(row, entry.factor);

        var info = new StackPanel { Orientation = Orientation.Vertical };
        Grid.SetColumn(info, 0);

        var publisher = new TextBlock { Text = entry.publisherActor, IsTextSelectionEnabled = true, TextTrimming = TextTrimming.CharacterEllipsis };
        AutomationProperties.SetAutomationId(publisher, Ids.LabelerCatalogItemPublisher);
        info.Children.Add(publisher);

        // The artifact kind ("list" | "wasm" | "text-model";
        // content-moderation-and-ranking.md § Tier-3 artifact kinds) —
        // distinguishes a curated List from an executable module BEFORE
        // inspect. Already normalized by the shared machine (absent/empty
        // wire ⇒ "wasm"); no client-side re-derivation. The ONE non-verbatim
        // field: a `text-model` whose tokenizer contract this build does not
        // implement overrides here instead, because the compose seam leaves
        // that factor inert and this row is where the user learns why. The
        // predicate and wording are both shared
        // (ValueFormat.TextModelNeedsNewerApp, over
        // fauna_core::format::text_model_needs_newer_app) — the same function
        // the compose seam's inert branch reads, so a badge that disagreed
        // with the scorer is not expressible here.
        var kindText = ValueFormat.TextModelNeedsNewerApp(entry.artifactKind, entry.artifactVersion) ?? entry.artifactKind;
        var kind = new TextBlock { Text = kindText, Opacity = 0.7, FontSize = 12 };
        AutomationProperties.SetAutomationId(kind, Ids.LabelerCatalogItemKind);
        info.Children.Add(kind);

        var contentKind = new TextBlock { Text = entry.contentKind, Opacity = 0.7, FontSize = 12 };
        AutomationProperties.SetAutomationId(contentKind, Ids.LabelerCatalogItemContentKind);
        info.Children.Add(contentKind);

        var version = new TextBlock { Text = entry.version.ToString(), Opacity = 0.7, FontSize = 12 };
        AutomationProperties.SetAutomationId(version, Ids.LabelerCatalogItemVersion);
        info.Children.Add(version);

        var factor = new TextBlock { Text = entry.factor, Opacity = 0.7, FontSize = 12, TextTrimming = TextTrimming.CharacterEllipsis };
        AutomationProperties.SetAutomationId(factor, Ids.LabelerCatalogItemFactor);
        info.Children.Add(factor);

        row.Children.Add(info);

        if (showInspectSubscribe)
        {
            var inspect = new Button { Content = S.Get("labeler_catalog/inspect") };
            AutomationProperties.SetAutomationId(inspect, Ids.LabelerCatalogItemInspectButton);
            Grid.SetColumn(inspect, 1);
            inspect.Click += async (_, _) => await onInspect(index);
            row.Children.Add(inspect);

            if (!entry.subscribed)
            {
                var subscribe = new Button
                {
                    Content = S.Get("labeler_catalog/subscribe"),
                    Style = (Style)Application.Current.Resources["AccentButtonStyle"],
                };
                AutomationProperties.SetAutomationId(subscribe, Ids.LabelerCatalogItemSubscribeButton);
                Grid.SetColumn(subscribe, 2);
                subscribe.Click += async (_, _) => await onSubscribe(index);
                row.Children.Add(subscribe);
            }
        }

        if (entry.subscribed)
        {
            var unsubscribe = new Button { Content = S.Get("labeler_catalog/unsubscribe") };
            AutomationProperties.SetAutomationId(unsubscribe, Ids.LabelerCatalogItemUnsubscribeButton);
            Grid.SetColumn(unsubscribe, 2);
            unsubscribe.Click += async (_, _) => await onUnsubscribe(index);
            row.Children.Add(unsubscribe);
        }

        return row;
    }
}
