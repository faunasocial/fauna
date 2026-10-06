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
/// a dumb renderer of the testable <see cref="MemberReviewViewModel"/>. windows
/// was the last of 7 apps still owing this permanent page; the ephemeral
/// kit-side pass is a separate, NOT-built-here arm (gated behind windows' own
/// identity-stolen ceremony legs).
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
    }

    private ConversationsManager Manager =>
        _clients?.ConvSession?.Manager() ?? ConversationsManagerHost.Instance;

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        try
        {
            if (_clients?.Rpc is null) return;
            _vm ??= new MemberReviewViewModel(_clients.Rpc);
            await _vm.LoadAsync(Manager);
            RenderState();
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
        ReviewList.ItemsSource = _vm.Reviews.ToList();

        ShowError(_vm.ErrorMessage);
    }

    private async void Keep_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button btn || btn.Tag is not byte[] person) return;
        await _vm.KeepAsync(person, Manager);
        RenderState();
    }

    private async void Remove_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button btn || btn.Tag is not byte[] person) return;
        await _vm.RemoveAsync(person, Manager);
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
