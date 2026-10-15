using System.Collections.Generic;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using FaunaApp.Core.Logs;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_feed;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// The Bluesky thread page (ui.yaml <c>feed.sub_pages.thread</c>, <c>bridges.md</c>
/// § Bluesky-native thread view): <c>feed-post-view-thread-button</c> on the post detail
/// opens it IN that detail's own dialog — WinUI shows one ContentDialog per XamlRoot, so
/// the page takes the dialog's place the way web's takes the detail's — and Back puts
/// the detail back. Every state is the shared <c>FeedSnapshot.thread</c>
/// (<c>FeedManager::open_thread</c>), repainted from the snapshot observer's ticks: one
/// <c>feed-thread-post</c> per post in thread order (ancestors oldest-first, the opened
/// post, its direct replies), the opened post's <c>selected</c> the nest's
/// <c>focal_index</c>, and a failed fetch's reason leading on the page's
/// <c>error-message</c> (<see cref="Refresh"/>).
/// </summary>
public sealed partial class FeedPage
{
    /// <summary>The post detail dialog open now, whichever door opened it (the post-card
    /// click or the search-nav deep link) — the dialog a thread page opens in.</summary>
    private ContentDialog? _detailDialog;

    /// <summary>The thread page open over <see cref="_detailDialog"/>, or <c>null</c>.</summary>
    private ThreadPageView? _thread;

    /// <summary>What the open thread page holds: the detail it stands in for (restored by
    /// Back) and the page's own panel, with what was last painted on it.</summary>
    private sealed class ThreadPageView(ContentDialog dialog, string postId, object detailTitle,
        object detailContent, StackPanel page)
    {
        public ContentDialog Dialog { get; } = dialog;
        public string PostId { get; } = postId;
        public object DetailTitle { get; } = detailTitle;
        public object DetailContent { get; } = detailContent;
        public StackPanel Page { get; } = page;
        public bool? PaintedLoading { get; set; }
        public IReadOnlyList<ThreadPostRow> PaintedRows { get; set; } = [];
    }

    /// <summary>Note <paramref name="dialog"/> as the open post detail. Closing it closes
    /// a thread page open in it too, so a slow fetch never lands on a page the reader
    /// left.</summary>
    private void TrackDetailDialog(ContentDialog dialog)
    {
        _detailDialog = dialog;
        dialog.Closed += (_, _) =>
        {
            if (!ReferenceEquals(_detailDialog, dialog)) return;
            CloseThreadPage(restoreDetail: false);
            _detailDialog = null;
        };
    }

    /// <summary><c>feed-post-view-thread-button</c> — offered only for a post the shared
    /// <c>post_offers_thread_view</c> admits (<see cref="FeedPostItem.OffersThreadView"/>).</summary>
    private Button BuildViewThreadButton(FeedPostItem item)
    {
        var button = new Button
        {
            Content = S.Get("feed/post/view_thread"),
            HorizontalAlignment = HorizontalAlignment.Left,
        };
        AutomationProperties.SetAutomationId(button, Ids.FeedPostViewThreadButton);
        button.Click += (_, _) => OpenThreadPage(item.PostId);
        return button;
    }

    private void OpenThreadPage(string postId)
    {
        if (_viewModel is null || _detailDialog is not { } dialog || _thread is not null) return;

        var page = new StackPanel { Spacing = 8 };
        AutomationProperties.SetAutomationId(page, Ids.FeedThreadPage);
        // A panel carrying only an AutomationId is pruned from the UIA tree, and the e2e
        // scopes every feed-thread-post read to this page — name it.
        AutomationProperties.SetName(page, S.Get("feed/post/thread"));

        // ui.yaml gives the page no dismiss element: Back is id-less, like web's ← button.
        var back = new Button
        {
            Content = S.Get("common/back"),
            HorizontalAlignment = HorizontalAlignment.Left,
        };
        back.Click += (_, _) => CloseThreadPage(restoreDetail: true);

        var root = new StackPanel { Spacing = 8 };
        root.Children.Add(back);
        root.Children.Add(new ScrollViewer { Content = page, MaxHeight = 400 });

        _thread = new ThreadPageView(dialog, postId, dialog.Title, dialog.Content, page);
        dialog.Title = S.Get("feed/post/thread");
        dialog.Content = root;
        PaintThreadPage();
        _ = OpenThreadAsync(postId);
    }

    private async System.Threading.Tasks.Task OpenThreadAsync(string postId)
    {
        try
        {
            await _viewModel!.OpenThreadAsync(postId);
        }
        catch (System.Exception ex)
        {
            // open_thread lands every fetch failure on the page's own state; only a fault
            // in the call itself reaches here — say so rather than leave the page loading.
            ShellLog.Warn(nameof(FeedPage), $"open_thread failed: {ex.Message}");
            ReportPageError(S.Format("feed/error_thread", ex.Message));
        }
    }

    /// <summary>Leave the thread page: the shared page closes (so a fetch still in flight
    /// lands nowhere) and, on Back, the post detail returns to the dialog.</summary>
    private void CloseThreadPage(bool restoreDetail)
    {
        if (_thread is not { } thread) return;
        _thread = null;
        _viewModel?.CloseThread();
        if (restoreDetail)
        {
            thread.Dialog.Title = thread.DetailTitle;
            thread.Dialog.Content = thread.DetailContent;
        }
        Refresh();
    }

    /// <summary>The open thread page's failure, while one is open — it leads on
    /// <c>error-message</c> ahead of the feed's own (tui's <c>page_error</c>).</summary>
    private string? OpenThreadPageError() =>
        _thread is { } thread && _viewModel?.Thread is { } page && page.postId == thread.PostId
            ? _viewModel.ThreadErrorText
            : null;

    /// <summary>Repaint the open thread page from the snapshot — a no-op when nothing it
    /// shows changed, so the frequent unrelated feed ticks leave its rows alone.</summary>
    private void PaintThreadPage()
    {
        if (_thread is not { } thread || _viewModel is null) return;
        var page = _viewModel.Thread is { } p && p.postId == thread.PostId ? p : null;
        var loading = page is null || page.status == ThreadStatus.Loading;
        IReadOnlyList<ThreadPostRow> rows = page is { status: ThreadStatus.Loaded }
            ? page.posts.Select(ThreadPostRow.From).ToList()
            : [];
        if (thread.PaintedLoading == loading && rows.SequenceEqual(thread.PaintedRows)) return;

        thread.Page.Children.Clear();
        if (loading)
            thread.Page.Children.Add(new TextBlock { Text = S.Get("common/loading"), Opacity = 0.6 });
        foreach (var row in rows)
            thread.Page.Children.Add(BuildThreadPostElement(row));
        thread.PaintedLoading = loading;
        thread.PaintedRows = rows;
    }

    /// <summary>One <c>feed-thread-post</c>: one text reading <c>‹author› ‹post text›</c>,
    /// its <c>selected</c> on HelpText (the one attribute it publishes, so unpacked — see
    /// <c>AutomationAttributePack</c>), the opened post set apart.</summary>
    private static FrameworkElement BuildThreadPostElement(ThreadPostRow row)
    {
        var text = new TextBlock { TextWrapping = TextWrapping.Wrap };
        text.Inlines.Add(new Run { Text = row.Author, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        text.Inlines.Add(new Run { Text = " " + row.Text });
        AutomationProperties.SetAutomationId(text, Ids.FeedThreadPost);
        // The UIA name is the row's one line, exactly — never left to how a TextBlock of
        // two Runs joins them.
        AutomationProperties.SetName(text, row.Line);
        AutomationProperties.SetHelpText(text, row.Selected);

        return new Border
        {
            Child = text,
            Padding = new Thickness(8, 4, 8, 4),
            CornerRadius = new CornerRadius(4),
            BorderThickness = new Thickness(row.IsFocal ? 3 : 0, 0, 0, 0),
            BorderBrush = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["AccentFillColorDefaultBrush"],
            Background = row.IsFocal
                ? (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SubtleFillColorSecondaryBrush"]
                : null,
        };
    }
}
