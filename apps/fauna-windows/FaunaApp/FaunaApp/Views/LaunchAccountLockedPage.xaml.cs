using System;
using System.ComponentModel;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// The locked launch surface — ui.yaml page <c>launch_account_locked</c> — and the
/// step it opens, <c>identity_stolen_entry</c> (<c>docs/goal/ui/sessions.md</c>
/// § Layout &amp; flow → <i>The locked surface (leg 2)</i>; <c>devices.md</c> § The
/// locked state). Shown when the launch machine's <c>Offline { transient: false }</c>
/// snapshot carries <c>lockedUntilSecs</c>; the arm in
/// <c>App.xaml.cs:DispatchLaunchSnapshotAsync</c> is checked AHEAD of the generic
/// offline split, which would paint a dead-end "update your nest" over it — the
/// order tui's <c>route()</c>, linux, android and FaunaKit take.
///
/// <para>A pure render over <see cref="LockedLaunchViewModel"/>. Following the
/// machine out (its own refresh at the unlock time, or a different lock) is the
/// shell's: it re-dispatches on the machine's ticks once
/// <see cref="LockedLaunchViewModel.NoticeHolds"/> says the notice no longer
/// stands.</para>
/// </summary>
public sealed partial class LaunchAccountLockedPage : Page
{
    private LaunchAccountLockedContext? _context;

    public LaunchAccountLockedPage()
    {
        InitializeComponent();
        TitleText.Text = S.Get("onboarding/launch/account_locked_title");
        StolenButton.Content = S.Get("settings/recovery_kit/stolen");
        StolenTitleText.Text = S.Get("settings/recovery_kit/stolen");
        StolenWarningText.Text = S.Get("settings/recovery_kit/stolen_warning");
        PhraseBox.PlaceholderText = S.Get("settings/recovery_kit/kit_phrase_placeholder");
        ConfirmBox.PlaceholderText = S.Get("settings/recovery_kit/stolen_confirm_placeholder");
        BackButton.Content = S.Get("common/back");
        SucceedButton.Content = S.Get("settings/recovery_kit/stolen");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is LaunchAccountLockedContext ctx)
        {
            _context = ctx;
            ctx.Model.PropertyChanged += Model_PropertyChanged;
            Render();
        }
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        if (_context is { } ctx)
        {
            ctx.Model.PropertyChanged -= Model_PropertyChanged;
            ctx.Left();
        }
        // The step's error surface belongs to this page only.
        App.CurrentErrorMessage = null;
        base.OnNavigatedFrom(e);
    }

    private void Model_PropertyChanged(object? sender, PropertyChangedEventArgs e) => Render();

    private void Render()
    {
        if (_context?.Model is not { } m) return;
        NoticeText.Text = m.NoticeText;
        NoticePanel.Visibility = m.InStolenEntry ? Visibility.Collapsed : Visibility.Visible;
        StolenPanel.Visibility = m.InStolenEntry ? Visibility.Visible : Visibility.Collapsed;
        if (PhraseBox.Password != m.Phrase) PhraseBox.Password = m.Phrase;
        if (ConfirmBox.Text != m.Confirm) ConfirmBox.Text = m.Confirm;
        SucceedButton.IsEnabled = m.Armed;
        BackButton.IsEnabled = !m.Busy;

        if (m.InStolenEntry && m.Error is { Length: > 0 } error)
        {
            ErrorBar.Message = error;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = error;
        }
        else
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
    }

    private void OnStolenClick(object sender, RoutedEventArgs e) => _context?.Model.OpenStolenEntry();

    private void OnBackClick(object sender, RoutedEventArgs e)
    {
        if (_context is not { } ctx) return;
        ctx.Model.CloseStolenEntry();
        // The notice stood unconditionally while the step was up; the machine may
        // have moved on meanwhile.
        ctx.Refollow();
    }

    private void PhraseBox_PasswordChanged(object sender, RoutedEventArgs e)
    {
        if (_context?.Model is { } m) m.Phrase = PhraseBox.Password;
    }

    private void ConfirmBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_context?.Model is { } m) m.Confirm = ConfirmBox.Text;
    }

    /// <summary>The ceremony re-checks both gates in its action arm, so a press
    /// that reached here past a disabled render still cannot run it.</summary>
    private async void OnSucceedClick(object sender, RoutedEventArgs e)
    {
        if (_context is not { } ctx) return;
        // Held in a local across the await: on the landed arm the switch re-roots
        // the frame, and this page is gone by the time it resumes.
        var model = ctx.Model;
        model.Phrase = PhraseBox.Password;
        model.Confirm = ConfirmBox.Text;
        await model.SucceedAsync(ctx.Switch);
    }
}

/// <summary>
/// Navigation parameter for <see cref="LaunchAccountLockedPage"/>: the surface's
/// state, the account switch that adopts a landed successor (each shell's ordinary
/// switch, re-running launch), the shell's re-read of the launch machine after Back,
/// and the shell's hook for the page going away (it stops following the machine).
/// </summary>
internal sealed record LaunchAccountLockedContext(
    LockedLaunchViewModel Model,
    Func<string, Task> Switch,
    Action Refollow,
    Action Left);
