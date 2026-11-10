using System;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Conversations;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using uniffi.fauna_conversations;

namespace FaunaApp.Views;

/// <summary>
/// The Settings → Members To Review sub-page (docs/goal/behavior/
/// succession-aftermath.md § Propagation) —
/// a dumb renderer of the testable <see cref="MemberReviewViewModel"/>. The rows
/// are <see cref="FaunaApp.Controls.MemberReviewList"/>, the same control the
/// ephemeral kit-side pass on <see cref="SettingsAccountPage"/>'s Recovery Kit
/// section renders — the two surfaces differ in their gate, not their markup.
///
/// <para>The manager resolution mirrors <c>ProfilePage.StartDm_Click</c>
/// exactly: the login session's wired manager in production,
/// <see cref="ConversationsManagerHost"/>'s fallback in E2E — the same
/// <see cref="ConversationsManager"/> instance every conversations-adjacent
/// surface on this page's session already shares, never a fresh one.</para>
/// </summary>
public sealed partial class SettingsMemberReviewPage : Page, IAsyncLoadedPage
{
    private ServiceClients? _clients;
    private MemberReviewViewModel? _vm;

    private TaskCompletionSource _loadComplete =
        new(TaskCreationOptions.RunContinuationsAsynchronously);

    /// <inheritdoc />
    public Task LoadComplete => _loadComplete.Task;

    public SettingsMemberReviewPage()
    {
        this.InitializeComponent();
        ReviewList.KeepRequested += Keep;
        ReviewList.RemoveRequested += Remove;
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
        }
        // Re-arm for a reused (cached) page instance so a second navigation waits on
        // this load, not the previous one's already-completed barrier.
        if (_loadComplete.Task.IsCompleted)
            _loadComplete = new(TaskCreationOptions.RunContinuationsAsynchronously);
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        _vm?.CleanupReconnect();
        if (_vm is null) return;
        _vm.CleanupStoreChange();
        _vm.StoreResynced -= RenderState;
    }

    /// <summary>Null once the handed-over session is retired (an actor change landed
    /// before the frame took this page away) — see
    /// <see cref="ConversationsManagerHost.ManagerFor"/>.</summary>
    private ConversationsManager? Manager =>
        ConversationsManagerHost.ManagerFor(_clients?.ConvSession, "member review");

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        try
        {
            if (_clients?.Rpc is null) return;
            _vm ??= new MemberReviewViewModel(_clients.Rpc);
            await _vm.LoadAsync(Manager);
            RenderState();
            // The shared store-change notice (account-runtime.md § A runtime's own
            // pump is a source of the notice too): a person put up for review
            // paints on this open page with no re-visit.
            _vm.StoreResynced -= RenderState;
            _vm.StoreResynced += RenderState;
            _vm.WatchStore(() => Manager);
        }
        finally
        {
            // ALWAYS complete, so the agent's bounded await never hangs a navigation.
            _loadComplete.TrySetResult();
        }
    }

    // ── Render ──

    private void RenderState()
    {
        if (_vm is null) return;

        var hasReviews = _vm.Reviews.Count > 0;
        // Neither the empty state nor the intro line paints while loading —
        // "nobody asked this nest to hold anything" must never stand in for
        // "the read has not answered yet" (the same honesty property this
        // row's admin-custody-hosting sibling pins).
        Intro.Visibility = !_vm.IsLoading && hasReviews ? Visibility.Visible : Visibility.Collapsed;
        EmptyState.Visibility = !_vm.IsLoading && !hasReviews ? Visibility.Visible : Visibility.Collapsed;
        ReviewList.Show(_vm.Reviews.ToList());

        ShowError(_vm.ErrorMessage);
    }

    private async void Keep(byte[] person)
    {
        if (_vm is null) return;
        await _vm.KeepAsync(person, Manager);
        RenderState();
    }

    private async void Remove(byte[] person)
    {
        if (_vm is null) return;
        if (Manager is not { } manager) return;
        await _vm.RemoveAsync(person, manager);
        RenderState();
    }

    private void ShowError(string? message)
    {
        if (string.IsNullOrEmpty(message))
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
        else
        {
            ErrorBar.Message = message;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = message;
        }
    }
}
