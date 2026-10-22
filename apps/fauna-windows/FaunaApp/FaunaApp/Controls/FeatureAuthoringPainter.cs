using FaunaApp.Core.Services;
using FaunaApp.Helpers;
using FaunaApp.UiIds;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// The element ids one host of the shared <c>feature-policy-editor</c> family
/// paints its rows with (<c>dynamic-features.md</c> § Authoring surfaces): the
/// admin Nest page and the Family page's guardian section share the row shape and
/// differ only in the id family; the Status page's rows carry their own
/// <c>feature-limits-own-*</c> pair inside the transparency row instead.
/// </summary>
internal sealed record FeatureHostIds(
    string Row, string Name, string Summary, string EditButton, string EditLabelKey)
{
    internal static FeatureHostIds Admin { get; } = new(
        Ids.AdminNestFeatureLimitsRow, Ids.AdminNestFeatureLimitsName,
        Ids.AdminNestFeatureLimitsSummary, Ids.AdminNestFeatureLimitsEditButton,
        "features/admin_edit");

    internal static FeatureHostIds Guardian { get; } = new(
        Ids.FamilyPolicyFeatureLimitsRow, Ids.FamilyPolicyFeatureLimitsName,
        Ids.FamilyPolicyFeatureLimitsSummary, Ids.FamilyPolicyFeatureLimitsEditButton,
        "features/admin_edit");
}

/// <summary>
/// The shared <c>feature-policy-editor</c> family's paint — ONE render for all
/// three of its hosts (the admin Nest page, a Status feature-limits row, the
/// Family page's guardian section). The twin of linux's <c>feature_editor.rs</c>,
/// tui's, web's and android's editor paints.
///
/// <para><b>This paints; it decides nothing.</b> The draft, the cell list and its
/// order, the labels, the no-effect notes, whether <i>Remove limit</i> renders and
/// the parser behind <i>Save</i> are all <see cref="FeatureAuthoringHost"/>'s
/// (shared Rust behind it), so the editor cannot differ between hosts or across
/// the 7 apps. The hosts differ only in the tier they open it at, which decides
/// the write kind the offline gate reads.</para>
///
/// <para>Rebuilt wholesale on every <see cref="FeatureAuthoringHost.Changed"/> by
/// the owning page; typing in a cell updates its note in place so the entry keeps
/// focus (the host raises nothing per keystroke).</para>
/// </summary>
internal static class FeatureAuthoringPainter
{
    /// <summary>One row per authored member of <paramref name="host"/>'s tier: name,
    /// the authored document in words, an Edit button, and — under the row whose
    /// editor is open — the editor itself.</summary>
    internal static StackPanel BuildRows(FeatureAuthoringHost host, FeatureHostIds ids)
    {
        var panel = new StackPanel { Spacing = 12 };
        if (host.State.Rows is not { } rows) return panel;

        foreach (var row in rows)
        {
            var rowName = S.Resolve(row.@name);
            var rowPanel = new StackPanel { Spacing = 4 };
            AutomationProperties.SetAutomationId(rowPanel, ids.Row);
            // A bare container with only an AutomationId is pruned from the UIA
            // tree (reference_winui_flaui_datatemplate_name) — name it.
            AutomationProperties.SetName(rowPanel, rowName);

            var head = new Grid { ColumnSpacing = 8 };
            head.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var text = new StackPanel { Spacing = 2 };
            var nameText = new TextBlock { Text = rowName, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
            AutomationProperties.SetAutomationId(nameText, ids.Name);
            text.Children.Add(nameText);
            var summaryText = new TextBlock
            {
                Text = S.Resolve(row.@summary),
                Opacity = 0.7,
                FontSize = 12,
                TextWrapping = TextWrapping.Wrap,
            };
            AutomationProperties.SetAutomationId(summaryText, ids.Summary);
            text.Children.Add(summaryText);
            head.Children.Add(text);

            var edit = EditButton(host, row.@feature, ids.EditButton, S.Get(ids.EditLabelKey));
            Grid.SetColumn(edit, 1);
            head.Children.Add(edit);
            rowPanel.Children.Add(head);

            if (host.State.OpenFeature == row.@feature && BuildEditor(host) is { } editor)
                rowPanel.Children.Add(editor);

            panel.Children.Add(rowPanel);
        }

        return panel;
    }

    /// <summary>A row's Edit button — opens the shared editor over
    /// <paramref name="feature"/> at the host's tier. Dispatches nothing over the
    /// wire (it reads the authored document), so it declares no offline kind.</summary>
    internal static Button EditButton(FeatureAuthoringHost host, string feature, string id, string label)
    {
        var button = new Button { Content = label, VerticalAlignment = VerticalAlignment.Center };
        AutomationProperties.SetAutomationId(button, id);
        button.Click += async (_, _) => await host.OpenAsync(feature);
        return button;
    }

    /// <summary>The open editor for <paramref name="host"/>, or <c>null</c> while
    /// none is open.</summary>
    internal static StackPanel? BuildEditor(FeatureAuthoringHost host)
    {
        var state = host.State;
        if (state.View is not { } view) return null;

        var title = S.ResolveNested(view.@title);
        var root = new StackPanel { Spacing = 6, Padding = new Thickness(12), Margin = new Thickness(0, 4, 0, 4) };
        AutomationProperties.SetAutomationId(root, Ids.FeaturePolicyEditor);
        AutomationProperties.SetName(root, title);

        var titleText = new TextBlock { Text = title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap };
        AutomationProperties.SetAutomationId(titleText, Ids.FeaturePolicyEditorTitle);
        root.Children.Add(titleText);

        // A one-of-two radio pair — exactly two choices, never a third "Limit":
        // whether On carries bounds is the cells' business. `state` rides HelpText
        // (the uniform get_attr(id, "state") idiom).
        var group = "feature-policy-editor-" + Guid.NewGuid().ToString("N");
        var onRadio = new RadioButton { GroupName = group, Content = S.Get("features/editor_on"), IsChecked = view.@on };
        var offRadio = new RadioButton { GroupName = group, Content = S.Get("features/editor_off"), IsChecked = !view.@on };
        AutomationProperties.SetAutomationId(onRadio, Ids.FeaturePolicyEditorOnRadio);
        AutomationProperties.SetAutomationId(offRadio, Ids.FeaturePolicyEditorOffRadio);
        AutomationProperties.SetHelpText(onRadio, view.@on ? "on" : "off");
        AutomationProperties.SetHelpText(offRadio, view.@on ? "off" : "on");
        // Attached after the seed above, so building the radios edits nothing.
        onRadio.Checked += (_, _) => host.SetOn(true);
        offRadio.Checked += (_, _) => host.SetOn(false);
        root.Children.Add(onRadio);
        root.Children.Add(offRadio);

        // An outer tier already denies: whatever is chosen here changes nothing,
        // and the editor says so rather than refusing anything.
        if (view.@offNote is { } offNote)
            root.Children.Add(DimText(S.Resolve(offNote)));
        var cells = view.@cells;
        if (cells.Length > 0)
            root.Children.Add(DimText(S.Resolve(view.@hint)));

        var notes = new List<TextBlock>();
        for (var i = 0; i < cells.Length; i++)
        {
            var index = i;
            var cell = cells[i];
            var label = S.ResolveNested(cell.@label);

            var cellPanel = new StackPanel { Spacing = 2 };
            AutomationProperties.SetAutomationId(cellPanel, Ids.FeaturePolicyEditorCell);
            AutomationProperties.SetName(cellPanel, label);

            var labelText = new TextBlock { Text = label };
            AutomationProperties.SetAutomationId(labelText, Ids.FeaturePolicyEditorCellLabel);
            cellPanel.Children.Add(labelText);

            var box = new TextBox { Text = cell.@text, MinWidth = 160, HorizontalAlignment = HorizontalAlignment.Left };
            AutomationProperties.SetAutomationId(box, Ids.FeaturePolicyEditorCellInput);
            AutomationProperties.SetName(box, label);
            cellPanel.Children.Add(box);

            // Present only when the ceiling already binds at or below the typed
            // value — the note is the shared editor's. Built always and toggled, so
            // typing never rebuilds the cell (and never drops the entry's focus).
            var note = DimText(string.Empty);
            AutomationProperties.SetAutomationId(note, Ids.FeaturePolicyEditorCellNote);
            PaintNote(note, S.EditorNoteText(cell));
            cellPanel.Children.Add(note);
            notes.Add(note);
            root.Children.Add(cellPanel);

            // Attached after the seed, so building the box edits nothing.
            box.TextChanged += (_, _) =>
            {
                if (host.SetCell(index, box.Text) is not { } updated) return;
                var updatedCells = updated.@cells;
                for (var n = 0; n < notes.Count && n < updatedCells.Length; n++)
                    PaintNote(notes[n], S.EditorNoteText(updatedCells[n]));
            };
        }

        // Save and Remove are OnlineOnly: the offline gate greys them from the
        // tier's own write kind, declared as literals so the offline-gate-kinds
        // checker can grade them against the shared table. Cancel is local and
        // stays live. Each button is in its panel BEFORE it is gated, so the gate's
        // reason caption has a parent to sit beside.
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        root.Children.Add(buttons);

        var save = new Button { Content = S.Get("features/editor_save") };
        if (Application.Current.Resources.TryGetValue("AccentButtonStyle", out var accent) && accent is Style accentStyle)
            save.Style = accentStyle;
        AutomationProperties.SetAutomationId(save, Ids.FeaturePolicyEditorSaveButton);
        save.Click += async (_, _) => await host.SaveAsync();
        buttons.Children.Add(save);

        Button? remove = null;
        if (view.@canRemove)
        {
            remove = new Button { Content = S.Get("features/editor_remove") };
            AutomationProperties.SetAutomationId(remove, Ids.FeaturePolicyEditorRemoveButton);
            remove.Click += async (_, _) => await host.RemoveAsync();
            buttons.Children.Add(remove);
        }

        var cancel = new Button { Content = S.Get("features/editor_cancel") };
        AutomationProperties.SetAutomationId(cancel, Ids.FeaturePolicyEditorCancelButton);
        cancel.Click += (_, _) => host.Cancel();
        buttons.Children.Add(cancel);

        switch (view.@tier)
        {
            case "admin":
                save.FaunaGate("fauna.features.policy.update");
                remove?.FaunaGate("fauna.features.policy.update");
                break;
            case "guardian":
                save.FaunaGate("fauna.family.policy.update");
                remove?.FaunaGate("fauna.family.policy.update");
                break;
            default:
                save.FaunaGate("fauna.features.self_limits.update");
                remove?.FaunaGate("fauna.features.self_limits.update");
                break;
        }

        if (state.Status is { } status)
        {
            var statusText = new TextBlock { Text = status, TextWrapping = TextWrapping.Wrap };
            AutomationProperties.SetAutomationId(statusText, Ids.FeaturePolicyEditorStatus);
            root.Children.Add(statusText);
        }

        return root;
    }

    private static void PaintNote(TextBlock note, string? text)
    {
        note.Text = text ?? string.Empty;
        note.Visibility = text is null ? Visibility.Collapsed : Visibility.Visible;
    }

    private static TextBlock DimText(string text) =>
        new() { Text = text, Opacity = 0.7, FontSize = 12, TextWrapping = TextWrapping.Wrap };
}
