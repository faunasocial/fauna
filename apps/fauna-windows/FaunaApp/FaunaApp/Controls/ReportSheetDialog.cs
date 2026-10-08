using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.UiIds;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// The shared report sheet (<c>report-sheet</c> — moderation.md § User-initiated
/// reporting → <i>App surface</i>): ONE dialog for every surface that opens it
/// (the feed post ⋯, the message ⋯, an OTHER profile), a dumb painter over
/// <see cref="ReportSheetViewModel"/>. Every word, the reason list and the submit
/// gate come from the shared <c>report_sheet_view</c> — nothing here decides.
///
/// <para>A ContentDialog (through the <see cref="Dialogs"/> gate, so an actor swap
/// force-closes it) carrying its own submit and cancel buttons — it has no
/// primary/close button of its own, because the submit is gated by the shared view
/// and must stay open on a failed send. The acknowledgement (<c>report-status</c>) is
/// NOT inside it: the caller paints it outside, after the sheet has closed
/// (<see cref="ReportStatusLine"/>).</para>
///
/// <para>Element names: each interactive control carries an
/// <c>AutomationProperties.Name</c> beside its id, or UIA prunes it
/// (reference_winui_flaui_datatemplate_name); the reason picker's items carry the
/// reason TOKEN as their Name — <c>driver.select</c> matches a ComboBoxItem's Name
/// exactly — and the label as Content.</para>
/// </summary>
internal static class ReportSheetDialog
{
    /// <summary>
    /// Open the sheet on <paramref name="target"/> and return how it ended: the
    /// <see cref="ReportOutcome"/> of the report that landed, or <c>null</c> when the
    /// user cancelled (or the open was refused because another dialog was up).
    /// <paramref name="onError"/> is the page's <c>error-message</c> line: a failed
    /// send reports there and keeps the sheet open for a retry.
    /// </summary>
    internal static async Task<ReportOutcome?> ShowAsync(
        XamlRoot xamlRoot, INestRpcClient rpc, FfiReportTarget target, Action<string> onError)
    {
        var vm = new ReportSheetViewModel(rpc, target);
        ReportOutcome? landed = null;

        var dialog = new ContentDialog { XamlRoot = xamlRoot };
        AutomationProperties.SetAutomationId(dialog, Ids.ReportSheet);

        var reasonBox = new ComboBox { HorizontalAlignment = HorizontalAlignment.Stretch };
        AutomationProperties.SetAutomationId(reasonBox, Ids.ReportReasonSelect);
        var noteBox = new TextBox
        {
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            MinHeight = 72,
        };
        AutomationProperties.SetAutomationId(noteBox, Ids.ReportNoteInput);
        var includeText = new CheckBox();
        AutomationProperties.SetAutomationId(includeText, Ids.ReportIncludeTextCheckbox);
        var blockAuthor = new CheckBox();
        AutomationProperties.SetAutomationId(blockAuthor, Ids.ReportBlockAuthorCheckbox);
        var blockedReason = new TextBlock { Opacity = 0.6, TextWrapping = TextWrapping.Wrap };
        var submit = new Button();
        AutomationProperties.SetAutomationId(submit, Ids.ReportSubmitButton);
        var cancel = new Button();
        AutomationProperties.SetAutomationId(cancel, Ids.ReportCancelButton);

        var initial = vm.View;
        foreach (var option in initial.reasons)
        {
            var label = S.Resolve(option.label);
            var item = new ComboBoxItem { Content = label, Tag = option.reason };
            AutomationProperties.SetName(item, option.reason);
            reasonBox.Items.Add(item);
        }

        var painting = false;
        void Repaint()
        {
            painting = true;
            try
            {
                var view = vm.View;
                dialog.Title = S.Resolve(view.title);
                reasonBox.Header = S.Resolve(view.reasonLabel);
                AutomationProperties.SetName(reasonBox, S.Resolve(view.reasonLabel));
                noteBox.Header = S.Resolve(view.noteLabel);
                AutomationProperties.SetName(noteBox, S.Resolve(view.noteLabel));
                // The excerpt checkbox renders only when the shared view says the
                // subject is sealed — a public post and an account have no text to
                // attach, and a hidden checkbox must not be countable.
                includeText.Visibility = view.showIncludeText ? Visibility.Visible : Visibility.Collapsed;
                includeText.Content = S.Resolve(view.includeTextLabel);
                AutomationProperties.SetName(includeText, S.Resolve(view.includeTextLabel));
                blockAuthor.Content = S.Resolve(view.blockAuthorLabel);
                AutomationProperties.SetName(blockAuthor, S.Resolve(view.blockAuthorLabel));
                var why = !view.canSubmit && view.blockedReason is { } r ? S.Resolve(r) : "";
                blockedReason.Text = why;
                blockedReason.Visibility = why.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
                submit.Content = S.Resolve(view.submitLabel);
                AutomationProperties.SetName(submit, S.Resolve(view.submitLabel));
                submit.IsEnabled = view.canSubmit && !vm.Sending;
                cancel.Content = S.Resolve(view.cancelLabel);
                AutomationProperties.SetName(cancel, S.Resolve(view.cancelLabel));
            }
            finally
            {
                painting = false;
            }
        }

        reasonBox.SelectionChanged += (_, _) =>
        {
            if (painting) return;
            vm.Reason = (reasonBox.SelectedItem as ComboBoxItem)?.Tag as string;
            Repaint();
        };
        noteBox.TextChanged += (_, _) =>
        {
            if (painting) return;
            vm.Note = noteBox.Text;
            Repaint();
        };
        includeText.Click += (_, _) =>
        {
            vm.IncludeText = includeText.IsChecked == true;
            Repaint();
        };
        blockAuthor.Click += (_, _) =>
        {
            vm.BlockAuthor = blockAuthor.IsChecked == true;
            Repaint();
        };
        cancel.Click += (_, _) => dialog.Hide();
        submit.Click += async (_, _) =>
        {
            submit.IsEnabled = false;
            var outcome = await vm.SubmitAsync();
            if (outcome.Sent)
            {
                landed = outcome;
                dialog.Hide();
                return;
            }
            // A failed send keeps the sheet for a retry and says why on the page.
            if (outcome.Error is { } error) onError(error);
            Repaint();
        };

        var panel = new StackPanel { Spacing = 8, MinWidth = 360 };
        panel.Children.Add(reasonBox);
        panel.Children.Add(noteBox);
        panel.Children.Add(includeText);
        panel.Children.Add(blockAuthor);
        panel.Children.Add(blockedReason);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        actions.Children.Add(submit);
        actions.Children.Add(cancel);
        panel.Children.Add(actions);
        dialog.Content = panel;
        Repaint();

        await Dialogs.ShowAsync(dialog);
        return landed;
    }
}
