using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Services;
using FaunaApp.UiIds;

namespace FaunaApp.Views.P2pShare;

/// <summary>
/// The peer-transfer plane's six-id surface on the Folders page (p2p.md § Cross-user
/// shared-set transfer) — <c>share-serve-status</c> plus the <c>share-transfer-list</c>'s
/// <c>share-transfer-item</c> rows. Render-only, and a removable build item with its XAML
/// (see <c>SharePlaneSection.xaml</c>).
///
/// <para>Reads <see cref="SharePlaneModel.Shared"/>; the model's <c>Changed</c> fires on a
/// Rust thread (the plane's repaint nudges), so the subscription marshals onto the
/// section's dispatcher. Every reading goes through <see cref="SharePlaneReadings"/>, which
/// resolves NESTED — "Limited by {source}" carries an i18n key as its argument.</para>
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> — WinUI rule, as on the host page.</para>
/// </summary>
public sealed partial class SharePlaneSection : UserControl
{
    public SharePlaneSection()
    {
        InitializeComponent();
        Loaded += (_, _) =>
        {
            SharePlaneModel.Shared.Changed += OnModelChanged;
            // The plane may have come up before this page instance existed (the page has no
            // NavigationCacheMode — a fresh one per visit): re-read, then paint.
            SharePlaneModel.Shared.Refresh();
        };
        Unloaded += (_, _) => SharePlaneModel.Shared.Changed -= OnModelChanged;
    }

    private void OnModelChanged()
    {
        if (DispatcherQueue.HasThreadAccess) Render();
        else DispatcherQueue.TryEnqueue(Render);
    }

    /// <summary>The single paint point. A <c>null</c> view — no plane running on this device
    /// this session — collapses the whole section: it owes the user no line, which is a
    /// different fact from the "no shared folders to serve" reading inside a non-null view.</summary>
    public void Render()
    {
        var view = SharePlaneModel.Shared.View;
        if (view is null)
        {
            SharePlaneRoot.Visibility = Visibility.Collapsed;
            ShareTransferListPanel.Children.Clear();
            return;
        }

        SharePlaneRoot.Visibility = Visibility.Visible;
        ShareServeStatusText.Text = SharePlaneReadings.ServeStatusText(view);

        var rows = SharePlaneReadings.TransferReadings(view);
        ShareTransferListPanel.Children.Clear();
        // Container carries the row count as its name, so it is in the UIA content view even
        // with zero rows (apple registers the same count as the list's value).
        AutomationProperties.SetName(ShareTransferListPanel, rows.Count.ToString());
        foreach (var row in rows) ShareTransferListPanel.Children.Add(BuildRow(row));
    }

    private static FrameworkElement BuildRow(SharePlaneTransferReading reading)
    {
        var row = new Grid { ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        AutomationProperties.SetAutomationId(row, Ids.ShareTransferItem);
        // Keeps the row in the UIA content view so FlaUI's ByAutomationId resolves the nested
        // child ids (reference_winui_flaui_datatemplate_name — the same trick every other
        // indexed container row on this page uses).
        AutomationProperties.SetName(row, reading.Name);

        // The three labels carry an AutomationId ONLY — a static Name would shadow the text
        // get_text reads (reference_windows_flaui_gettext_dynamic_label_no_name).
        row.Children.Add(Label(Ids.ShareTransferName, reading.Name, 0));
        row.Children.Add(Label(Ids.ShareTransferProgress, reading.Progress, 1, dim: true));
        row.Children.Add(Label(Ids.ShareTransferState, reading.State, 2));
        return row;
    }

    private static TextBlock Label(string id, string text, int column, bool dim = false)
    {
        var label = new TextBlock
        {
            Text = text,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            TextWrapping = TextWrapping.NoWrap,
            Opacity = dim ? 0.7 : 1.0,
        };
        AutomationProperties.SetAutomationId(label, id);
        Grid.SetColumn(label, column);
        return label;
    }
}
