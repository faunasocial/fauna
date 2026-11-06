using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.UiIds;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using uniffi.fauna_ffi;

namespace FaunaApp.Controls;

/// <summary>
/// The shared <c>rule-editor</c> component's paint on windows (<c>nest-rules.md</c>
/// § The shared rule editor → <i>Three mountings, one shape</i>) — ONE control for
/// every mounting: <c>AdminNestPage</c> hosts it in <c>admin-nest-rules-section</c>,
/// <c>FamilyPage</c> in <c>family-policy-rules-section</c> for the selected ward.
/// The twin of tui's <c>rule_editor.rs</c>, android's <c>RuleEditor.kt</c> and apple's
/// <c>RuleEditorPanel.swift</c>.
///
/// <para><b>This paints; it decides nothing.</b> Every decision is the shared
/// <see cref="FfiRuleEditor"/>'s, held by <see cref="RuleEditorModel"/>; a host owns
/// the editor handle (<see cref="Load"/>), the save (<see cref="SaveHandler"/>, whose
/// answer is the status line) and the offline gate on <see cref="SaveButtonControl"/>,
/// declared with its own write kind.</para>
///
/// <para><b>When the rows are rebuilt.</b> Every edit is applied to the editor at once;
/// whatever it changes on screen is painted on the NEXT dispatcher pass, never inside
/// the control's own <c>SelectionChanged</c>/<c>Click</c>: a control replaced or
/// disabled synchronously inside its own event makes the FlaUI bridge's
/// <c>SelectionItem.Select</c> report <c>ElementNotEnabledException</c> although the
/// pick landed. A structural edit (add,
/// remove, a kind or labeler pick) rebuilds the rows; a category, verdict or level pick
/// changes no control but its own, so only the save button and the status line follow
/// it. Typing never rebuilds: each <c>TextChanged</c> goes to the editor and only that
/// frame follows, so the box being typed in keeps its focus and caret.</para>
/// </summary>
public sealed partial class RuleEditorPanel : UserControl
{
    private readonly RuleEditorModel _model = new();

    /// <summary>Set while <see cref="Paint"/> builds controls, so a programmatic
    /// selection or text never reads back as the user's edit.</summary>
    private bool _painting;

    private bool _repaintQueued;
    private bool _repaintFull;
    private int _labelerReads;

    public RuleEditorPanel()
    {
        InitializeComponent();
        Paint();
    }

    /// <summary>The connected session the labeler picker reads through
    /// (<see cref="INestRpcClient.RuleLabelersAsync"/> / <see cref="INestRpcClient.RuleModuleAsync"/>).</summary>
    internal INestRpcClient? Rpc { get; set; }

    /// <summary>The host's save: handed <see cref="FfiRuleEditor.DocumentBytes"/>,
    /// it answers the status line's text (and, for the admin, the editor reloaded from
    /// what the nest stored). Called only while the view's <c>canSave</c> holds.</summary>
    internal Func<byte[], Task<RuleEditorSaveOutcome>>? SaveHandler { get; set; }

    /// <summary><c>rule-editor-save-button</c>, for the host's offline-gate
    /// declaration — each host spells its own write kind as a literal at its call
    /// site (<c>fauna.admin.content_policy.set</c> | <c>fauna.family.policy.update</c>).</summary>
    internal Button SaveButtonControl => SaveButton;

    /// <summary>The working editor, or <c>null</c> while the host's read is in flight.</summary>
    internal FfiRuleEditor? Editor => _model.Editor;

    /// <summary>Replace the working document — a fresh read or a landed save
    /// (<c>null</c>: the loading state). The host's status is cleared; the
    /// subscribed labelers are read again beside the new document.</summary>
    internal void Load(FfiRuleEditor? editor)
    {
        _model.Load(editor);
        QueueRepaint(full: true);
        if (editor is not null) _ = RefreshLabelersAsync(editor);
    }

    /// <summary>Set <c>rule-editor-status</c> to the host's verdict (<c>null</c>:
    /// the editor's own defects show).</summary>
    internal void SetStatus(string? status)
    {
        _model.Status = status;
        QueueRepaint(full: false);
    }

    /// <summary>The document to save, or <c>null</c> while it cannot be saved.</summary>
    internal byte[]? DocumentBytes()
    {
        try
        {
            return _model.Editor?.DocumentBytes();
        }
        catch (FfiException)
        {
            return null;
        }
    }

    private async Task RefreshLabelersAsync(FfiRuleEditor editor)
    {
        if (Rpc is not { } rpc) return;
        var read = ++_labelerReads;
        var subscribed = await RuleEditorHosts.SubscribedLabelersAsync(rpc);
        if (read != _labelerReads || !ReferenceEquals(editor, _model.Editor)) return;
        _model.Subscribed = subscribed;
        // Only a labeler row's picker paints the list; rebuilding anything else
        // would take the caret from a box being typed in.
        if (_model.HasLabelerRow) QueueRepaint(full: true);
    }

    // ── Repaint scheduling ───────────────────────────────────────────────

    /// <summary>Repaint on the next dispatcher pass — <paramref name="full"/>
    /// rebuilds the rows, otherwise only the frame (save button, status line)
    /// follows the model. Coalesced; a pending full repaint absorbs a lighter one.</summary>
    private void QueueRepaint(bool full)
    {
        _repaintFull |= full;
        if (_repaintQueued) return;
        _repaintQueued = true;
        var queued = DispatcherQueue.TryEnqueue(() =>
        {
            var rebuild = _repaintFull;
            _repaintQueued = false;
            _repaintFull = false;
            if (rebuild) Paint();
            else PaintFrame(_model.Paint());
        });
        if (!queued)
        {
            _repaintQueued = false;
            _repaintFull = false;
        }
    }

    // ── Paint ────────────────────────────────────────────────────────────

    private void Paint()
    {
        _painting = true;
        try
        {
            var paint = _model.Paint();
            RowsHost.Children.Clear();
            foreach (var row in paint.Rows) RowsHost.Children.Add(BuildRow(row));
            NewerVersionText.Visibility = paint.Loaded && !paint.Editable ? Visibility.Visible : Visibility.Collapsed;
            NoRulesText.Visibility = paint.Loaded && paint.Rows.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
            ActionsRow.Visibility = paint.Loaded ? Visibility.Visible : Visibility.Collapsed;
            AddButton.IsEnabled = paint.AddEnabled;
            PaintFrame(paint);
        }
        finally
        {
            _painting = false;
        }
    }

    /// <summary>The controls every edit's dependents live in: whether the document
    /// may be saved, and the status line.</summary>
    private void PaintFrame(RuleEditorPaint paint)
    {
        SaveButton.IsEnabled = paint.SaveEnabled;
        SaveButton.Content = paint.SaveLabel;
        StatusText.Text = paint.Status;
        StatusText.Visibility = string.IsNullOrEmpty(paint.Status) ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>One <c>rule-editor-item</c>: the kind picker, exactly one subject
    /// control by kind (the others are never built — absent, not hidden), the verdict,
    /// the level when the rule has one, the default-language reason and Remove.</summary>
    private StackPanel BuildRow(RuleRowPaint row)
    {
        var panel = new StackPanel { Spacing = 4, Padding = new Thickness(0, 4, 0, 4) };
        AutomationProperties.SetAutomationId(panel, Ids.RuleEditorItem);
        // A bare container with only an AutomationId is pruned from the UIA tree
        // (reference_winui_flaui_datatemplate_name) — name it, or the scoped
        // `rule-editor-item[i]` reads resolve nothing.
        AutomationProperties.SetName(panel, row.Title);
        panel.Children.Add(new TextBlock { Text = row.Title, Opacity = 0.7, FontSize = 12 });

        if (row.ReadOnly)
        {
            panel.Children.Add(new TextBlock { Text = row.KindLabel, TextWrapping = TextWrapping.Wrap });
        }
        else
        {
            var index = row.Index;
            panel.Children.Add(Picker(
                Ids.RuleEditorItemKindSelect, "rule_editor/kind_label",
                row.KindOptions, row.KindSelected, row.Live,
                option => Structural(_model.PickKind(index, option))));
            if (BuildSubject(row) is { } subject) panel.Children.Add(subject);
            panel.Children.Add(Picker(
                Ids.RuleEditorItemVerdictSelect, "rule_editor/verdict_label",
                row.VerdictOptions, row.VerdictSelected, row.Live,
                option => InPlace(_model.PickVerdict(index, option))));
            if (row.LevelOptions is { } levels)
                panel.Children.Add(Picker(
                    Ids.RuleEditorItemLevelSelect, "rule_editor/level_label",
                    levels, row.LevelSelected, row.Live,
                    option => InPlace(_model.PickLevel(index, option))));
            panel.Children.Add(Input(
                Ids.RuleEditorItemReasonInput, "rule_editor/reason_label",
                row.Reason, row.Live, text => _model.SetReason(index, text)));
        }

        if (row.Inert is { } inert)
            panel.Children.Add(new TextBlock { Text = inert, Opacity = 0.7, FontSize = 12, TextWrapping = TextWrapping.Wrap });

        var remove = new Button
        {
            Content = Strings.Get("rule_editor/remove_button"),
            IsEnabled = row.RemoveEnabled,
        };
        AutomationProperties.SetAutomationId(remove, Ids.RuleEditorItemRemoveButton);
        var removeIndex = row.Index;
        remove.Click += (_, _) => Structural(_model.RemoveRule(removeIndex));
        panel.Children.Add(remove);
        return panel;
    }

    private FrameworkElement? BuildSubject(RuleRowPaint row)
    {
        var index = row.Index;
        var subject = row.Subject;
        return subject.Kind switch
        {
            RuleSubject.Category => Picker(
                Ids.RuleEditorItemCategorySelect, "rule_editor/category_label",
                subject.Options, subject.Selected, row.Live,
                option => InPlace(_model.PickCategory(index, option))),
            RuleSubject.Keywords => Input(
                Ids.RuleEditorItemKeywordsInput, "rule_editor/keywords_label",
                subject.Text, row.Live, text => _model.SetKeywords(index, text)),
            RuleSubject.Labeler => Picker(
                Ids.RuleEditorItemLabelerSelect, "rule_editor/labeler_label",
                subject.Options, subject.Selected, row.Live,
                option => _ = PickLabelerAsync(index, option)),
            RuleSubject.Prompt => Input(
                Ids.RuleEditorItemPromptInput, "rule_editor/prompt_label",
                subject.Text, row.Live, text => _model.SetPrompt(index, text)),
            _ => null,
        };
    }

    /// <summary>A structural edit was applied to the editor (or refused): rebuild on
    /// the next pass — which also puts a refused pick's picker back on the stored
    /// value. Never synchronously: the control that raised the edit is replaced.</summary>
    private void Structural(bool _) => QueueRepaint(full: true);

    /// <summary>A pick that changes no control but its own (category, verdict,
    /// level): the picker already shows it, so only the frame follows, on the next
    /// pass — fewer replacements for the next UIA call to race. A refused pick
    /// rebuilds instead, putting the picker back on the stored value.</summary>
    private void InPlace(bool changed) => QueueRepaint(full: !changed);

    private async Task PickLabelerAsync(int row, int option)
    {
        var rpc = Rpc;
        await _model.PickLabelerAsync(row, option, id =>
            rpc is null ? Task.FromResult<byte[]?>(null) : rpc.RuleModuleAsync(id));
        QueueRepaint(full: true);
    }

    /// <summary>
    /// A cross-app LABEL picker (ui.yaml types every <c>rule-editor-item-*-select</c>
    /// as <c>select</c>): each option is the label the crate painted, and each
    /// <c>ComboBoxItem</c>'s UIA Name is exactly that label — the windows bridge's
    /// <c>select(id, label)</c> matches an item's Name exactly, and <c>get_text</c>
    /// reads the selected item back.
    /// </summary>
    private ComboBox Picker(
        string id, string headerKey, IReadOnlyList<string> options, int selected, bool enabled,
        Action<int> onPick)
    {
        var combo = new ComboBox
        {
            Header = Strings.Get(headerKey),
            MinWidth = 220,
            IsEnabled = enabled,
        };
        AutomationProperties.SetAutomationId(combo, id);
        foreach (var label in options)
        {
            var item = new ComboBoxItem { Content = label };
            AutomationProperties.SetName(item, label);
            combo.Items.Add(item);
        }
        if (selected >= 0 && selected < options.Count) combo.SelectedIndex = selected;
        var current = combo.SelectedIndex;
        combo.SelectionChanged += (_, _) =>
        {
            if (_painting) return;
            var picked = combo.SelectedIndex;
            // A re-raise of the value this picker was painted with is no edit.
            if (picked < 0 || picked == current) return;
            current = picked;
            onPick(picked);
        };
        return combo;
    }

    /// <summary>A single-line per-rule input. Each change goes to the editor and only
    /// the frame follows; the box itself is never rebuilt while it is typed in. A
    /// <c>TextChanged</c> echoing the painted text (WinUI raises it late for a
    /// programmatic <c>Text</c>) changes nothing in the model, so it is no edit.</summary>
    private TextBox Input(string id, string headerKey, string text, bool enabled, Func<string, bool> onText)
    {
        var box = new TextBox
        {
            Header = Strings.Get(headerKey),
            Text = text,
            IsEnabled = enabled,
            AcceptsReturn = false,
            TextWrapping = TextWrapping.NoWrap,
            MinWidth = 280,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        AutomationProperties.SetAutomationId(box, id);
        box.TextChanged += (_, _) =>
        {
            if (_painting) return;
            if (onText(box.Text)) PaintFrame(_model.Paint());
        };
        return box;
    }

    // ── The frame's two buttons ──────────────────────────────────────────

    private void AddButton_Click(object sender, RoutedEventArgs e) => Structural(_model.AddRule());

    private async void SaveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_model.Saving || SaveHandler is not { } save) return;
        byte[] document;
        try
        {
            if (_model.Editor?.DocumentBytes() is not { } bytes) return;
            document = bytes;
        }
        catch (Exception ex)
        {
            _model.Status = Strings.Error(ex);
            QueueRepaint(full: false);
            return;
        }
        // The busy flag at once; the button's in-flight disable on the next pass,
        // never inside its own Click.
        _model.Saving = true;
        QueueRepaint(full: false);
        RuleEditorSaveOutcome outcome;
        try
        {
            outcome = await save(document);
        }
        catch (Exception ex)
        {
            outcome = new RuleEditorSaveOutcome(Strings.Error(ex));
        }
        finally
        {
            _model.Saving = false;
        }
        if (outcome.Reload is { } reload) Load(reload);
        // After any reload — the host's own re-read included — so what the status
        // says is about the document now painted.
        _model.Status = outcome.Status;
        QueueRepaint(full: true);
    }
}
