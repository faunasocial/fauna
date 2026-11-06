using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using FaunaApp.Services;
using FaunaApp.Sync;
using FaunaApp.Views;
using uniffi.fauna_conversations;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Feed;

/// <summary>
/// What a page that hosts post-cards hands the cards' <see cref="PostCardHost"/>: the
/// feed manager its rows came from and the page's own surfaces the cards' verbs paint
/// on. Implemented by the feed page (the timeline) and the profile page (the Posts
/// tab's author window).
/// </summary>
internal interface IPostCardPage
{
    /// <summary>The view model over the manager the rows came from — the session's
    /// timeline on the feed page, the author window on the profile; null until the page
    /// has built it.</summary>
    FeedViewModel? CardViewModel { get; }
    INestRpcClient? Rpc { get; }
    ICryptoService? Crypto { get; }
    ISessionAccount? Account { get; }
    /// <summary>The root the cards' dialogs and sheets open in.</summary>
    XamlRoot XamlRoot { get; }
    /// <summary>A failure that stays on the page's <c>error-message</c> until a later
    /// verb lands or the page reloads.</summary>
    void ReportPageError(string message);
    /// <summary>A verb landed: clear a previous verb's sticky error, if one is up.</summary>
    void ClearPageError();
    /// <summary>A failure painted straight onto <c>error-message</c>, which the page's
    /// next repaint from its snapshot may clear.</summary>
    void ShowError(string message);
    /// <summary>The report acknowledgement (<c>report-status</c>); null hides it.</summary>
    void ShowReportStatus(string? acknowledgement);
    /// <summary>Re-read the rows from the manager now (a report changed the hide list).</summary>
    void RefreshCards();
    /// <summary>Open the shared lightbox on a card's already-loaded picture.</summary>
    void ShowImageLightbox(BitmapImage image);
    /// <summary>The card's activation (ui.yaml <c>click post-card → post_detail</c>): the
    /// feed page opens its detail dialog; the profile opens the feed page's detail
    /// through the door a search result uses.</summary>
    Task OpenPostDetailAsync(FeedPostItem item);
}

/// <summary>
/// The behaviour behind every <c>post-card</c> one list paints — ONE implementation for
/// the feed page's post list and the profile page's Posts tab
/// (`ui/profile.md` § Layout &amp; flow → <i>The Posts tab</i>: "the post-card component
/// with every child it has on the feed … in identical paint"; ui.yaml <c>post-card</c>
/// <c>used_in: [feed, profile]</c>). Lifted from <c>FeedPage.xaml.cs</c>, never copied.
///
/// <para>A page builds one over its <see cref="ListView"/> and hands it the
/// <see cref="IPostCardPage"/> that names the manager the rows came from. Every verb
/// acts on THAT manager — the timeline's on the feed, the author window's on the
/// profile — so a like, repost, quote, buy, reveal, reply, train, delete or web-publish
/// made on the profile moves the counts the profile paints (tui's
/// <c>feed::apply_on</c>, linux's card-manager parameter, apple's window-bound
/// <c>FeedVM</c>). The shared template (<c>Controls/PostCardTemplates.xaml</c>) finds
/// the host of the list a card sits in through <see cref="Of"/>.</para>
///
/// <para>Owns the bound <see cref="Posts"/> projection (<see cref="Sync"/> reconciles it
/// against each snapshot and kicks off the lazy per-post resolves), the ⋯ menu, the
/// reply dialog, the per-card payments plane and the tip list window.</para>
///
/// <para>No <c>ConfigureAwait(false)</c> anywhere here: the continuations mutate bound
/// state, which off the UI thread throws a silent COMException
/// (reference_windows_vm_configureawait_comexception).</para>
/// </summary>
internal sealed class PostCardHost
{
    /// <summary>Set on the host's list so a card's handler finds its host by walking up.</summary>
    private static readonly DependencyProperty HostProperty = DependencyProperty.RegisterAttached(
        "PostCardHostSlot", typeof(object), typeof(ListViewBase), new PropertyMetadata(null));

    /// <summary>The host of the list <paramref name="sender"/> sits in, or null.</summary>
    public static PostCardHost? Of(object sender)
    {
        for (var node = sender as DependencyObject; node is not null; node = VisualTreeHelper.GetParent(node))
        {
            if (node is ListViewBase list && list.GetValue(HostProperty) is PostCardHost host) return host;
        }
        return null;
    }

    private readonly IPostCardPage _page;
    private readonly ListViewBase _list;
    private readonly string _witnessSurface;

    /// <summary>Lazily-built, host-lived <c>FfiWebClient</c> for the ⋯-menu's own-post
    /// web-publishing verbs (web-content-hosting.md § Published-post management) —
    /// built over the shared, auto-reconnecting WS-RPC connection (mirrors
    /// <c>SettingsWebPage.EnsureViewModelAsync</c>), never a per-open one-shot
    /// connection.</summary>
    private uniffi.fauna_ffi.FfiWebClient? _webClient;

#if PAYMENTS
    /// <summary>The flat tip-attribution window, built in code and dropped into the
    /// page's tip-list host — dynamic-features.md § Platform-family surface excision.
    /// Null when the page gave no host.</summary>
    private readonly Views.Payments.PostTipListDialog? _tipListDialog;
#endif

    /// <summary>The page's bound render projection of the snapshot (reconciled each tick).</summary>
    public ObservableCollection<FeedPostItem> Posts { get; } = new();

    /// <param name="witnessSurface">The surface prefix of convention 17's region / nest
    /// block witness keys (<c>"feed"</c>, <c>"profile"</c>), so one post painted on two
    /// surfaces is two witnesses.</param>
    /// <param name="tipListDialogHost">The page's overlay host for the tip list window,
    /// filled under <c>PAYMENTS</c>.</param>
    public PostCardHost(IPostCardPage page, ListViewBase list, string witnessSurface, ContentControl? tipListDialogHost)
    {
        _page = page;
        _list = list;
        _witnessSurface = witnessSurface;
        list.SetValue(HostProperty, this);
        list.ItemsSource = Posts;
#if PAYMENTS
        // §§ TipDisplayHost / the tip list window — the payments plane lives in its
        // own removable build item (dynamic-features.md § Platform-family surface
        // excision); wired the way ProfilePage wires PaymentsAuthorSections, one seam
        // further out since this one is per-card rather than a single section.
        if (tipListDialogHost is not null)
        {
            _tipListDialog = new Views.Payments.PostTipListDialog();
            tipListDialogHost.Content = _tipListDialog;
        }
        list.ContainerContentChanging += List_ContainerContentChanging;
#endif
    }

    private FeedViewModel? ViewModel => _page.CardViewModel;
    private INestRpcClient? Rpc => _page.Rpc;
    private ICryptoService? Crypto => _page.Crypto;
    private ISessionAccount? Account => _page.Account;

    /// <summary>Rebuild the post-card rows from the snapshot's ordered, deduplicated
    /// post list, kicking off the lazy per-post media + quoted-post resolution
    /// (mirrors linux render_posts).</summary>
    internal void Sync(IReadOnlyList<PostSummary> snapshot)
    {
        // Reconcile the bound rows against the snapshot IN PLACE: an UNCHANGED post keeps its
        // existing FeedPostItem — and so its already-realized container plus any in-flight
        // /api/v1/blob/<hash> image load — while only a post whose rendered content actually
        // changed (media/quote/link-preview folded into the document, counts bumped, a remote
        // image revealed) rebuilds its row to paint the change. The prior Posts.Clear()+rebuild
        // every observer tick tore down a stable post's in-flight image fetch before it settled
        // (each fire-once resolve trigger below re-emits a notify → Refresh → SyncPosts), so the
        // post-image never appeared — the feed image-render gap (render-model.md § D6; feed.md
        // § State & data shape). Generalizes the SyncFeeds/SyncAvailableBridges id-diff guard
        // to per-row content via FeedPostItem.ContentEquals.
        var target = new List<FeedPostItem>(snapshot.Count);
        var bridgeRoster = ViewModel?.BridgeRoster;
        foreach (var p in snapshot)
            target.Add(new FeedPostItem(p, ViewModel?.Manager, bridgeRoster));
        ObservableCollectionReconcile.Reconcile(
            Posts, target, item => item.PostId, static (a, b) => a.ContentEquals(b));

        // Kick off the lazy per-post media + quoted-post + link-preview resolution. Each trigger
        // is fire-once (the guards below); the manager folds the resolved block + re-emits, and
        // the reconcile above then rebuilds ONLY that post's row to paint the embed, leaving
        // every sibling row (and its in-flight load) in place.
        foreach (var p in snapshot)
        {
            // Trigger media resolution: the manager resolves the blob hash, folds an `Image`
            // block into the document + re-emits → next Refresh rebuilds the row and paints the
            // post-image from the document (render-model.md § D6). Fire-once on the unresolved
            // sibling hash so the notify loop terminates once it's set.
            if (p.hasMedia && p.mediaHash is null && ViewModel is not null)
                _ = ViewModel.ResolveMediaAsync(p.postId);
            // Trigger the quoted-post fold: the manager folds a `QuotedPost` block into the
            // document + re-emits **idempotently** → next Refresh repaints the quoted-post card
            // from the document. Fire-once — skip once the block is already folded (mirrors
            // linux/android; the idempotent re-emit is the loop-safety backstop, not the gate).
            if (p.quotedPostId is { } qid && DocumentRenderer.QuotedPost(p.document) is null && ViewModel is not null)
                _ = ViewModel.ResolveQuotedPostAsync(qid);
            // A REPOST ROW's original folds in through the SAME QuotedPost embed —
            // resolve_quoted_post resolves either field, so a repost target needs no new
            // render machinery, just the same fire-once trigger keyed on reposted_post_id
            // instead of quoted_post_id (feed.md § Interaction bar → Repost, ratified
            // 2026-08-10).
            if (p.repostedPostId is { } rid && DocumentRenderer.QuotedPost(p.document) is null && ViewModel is not null)
                _ = ViewModel.ResolveQuotedPostAsync(rid);
            // Trigger link-preview resolution (render-model.md § D4): for EACH folded
            // `LinkPreview` block still in `Resolving`, the shared
            // `FeedManager::resolve_link_preview` resolves the url + re-emits → next Refresh
            // paints a `link-preview-card` per `Resolved` state. Fire-once on the `Resolving`
            // state — once terminal the block is `Resolved`/`Failed`, so this won't re-fire (the
            // cached, idempotent resolve is the in-flight backstop, mirroring the media/quoted
            // triggers above). The shared `resolving_link_preview_urls` face yields every
            // unresolved url in body order, so a post with two bare URLs resolves BOTH — the
            // single-preview `FirstOrDefault` twin this replaced would have stranded the second
            // in `Resolving` forever (mirrors linux's identical loop).
            if (ViewModel is not null)
            {
                foreach (var url in DocumentRenderer.ResolvingLinkPreviewUrls(p.document))
                    _ = ViewModel.ResolveLinkPreviewAsync(url);
            }
            // Trigger the sold-post buyer teaser resolve (gap (2c), monetization.md § Per-post
            // pay-to-unlock): the manager folds a resolved offer into `unlock_offer` + re-emits
            // → next Refresh paints the card's SoldPostTeaser (a payments build's only; the
            // resolve itself is a subscriptions read every flavor makes, as on tui and apple,
            // and FeedPostItem's offer fields stay ungated inert data). Fire-once on
            // unresolved — the shared resolve_post_unlock_offer itself no-ops (no I/O, no
            // re-emit) unless gatedTier names a post-unlock-* tier, so gating on "any gated
            // post with no offer yet" (rather than duplicating the tier-prefix check here) is
            // correct AND cheap, matching linux's identical trigger.
            if (p.gatedTier is not null && p.unlockOffer is null && ViewModel is not null)
                _ = ViewModel.ResolvePostUnlockOfferAsync(p.postId);
            // The remote author's byline: a delivered post homed on another nest whose
            // author face is still unresolved asks the manager once; every no-face
            // outcome writes an empty view, so this guard closes (feed.md § The remote
            // author's byline). The card label itself changes nowhere.
            if (p.homeNestUrl is { Length: > 0 } && p.authorDisplay is null && ViewModel is not null)
                _ = ViewModel.ResolveAuthorFaceAsync(p.postId);
#if PAYMENTS
            // Trigger the tip attribution resolve (monetization.md § Tips): no
            // "does this post have tips" signal exists in the feed projection (unlike
            // gatedTier above), so this fires for every post. The shared resolver
            // writes a view on every outcome — including "no tips" and a transport
            // error — so `tips == null` is a safe, terminating fire-once
            // gate (mirrors ResolveMediaAsync). Gated: the resolve itself sends a
            // wire kind excised entirely in a store-safe deployment (mirrors
            // FeedViewModel.ResolvePostTipsAsync's own guard) — reading/displaying an
            // already-resolved p.tips is NOT gated.
            if (p.tips is null && ViewModel is not null)
                _ = ViewModel.ResolvePostTipsAsync(p.postId);
#endif   // PAYMENTS
            // Trigger the C2PA-provenance check (media.md § C2PA provenance): the SAME
            // authenticated blob GET the list-card image already performs also returns
            // the `x-c2pa` header (BlobImageLoader.LoadWithC2paAsync shares its cache
            // with the plain LoadAsync the image binding calls below — no extra
            // request). Fire-once via FeedPostItem's own checked flag: unlike every
            // trigger above, there is no manager/snapshot-side signal to gate on — this
            // is pure client render glue, the same class as linux/android/apple's own
            // per-app c2pa-badge wiring. Looked up post-Reconcile so the check lands on
            // the STABLE row instance a content-unchanged tick kept (never the
            // throwaway `target` item Reconcile may have discarded).
            if (p.hasMedia && p.mediaHash is { Length: > 0 } mediaHash && PostCardBind.GetImageLoader() is { } imgLoader)
            {
                var row = Posts.FirstOrDefault(i => i.PostId == p.postId);
                if (row is { C2paChecked: false })
                    _ = TriggerC2paCheckAsync(row, imgLoader, mediaHash);
            }
        }

#if PAYMENTS
        // Tip state or an unlock offer that arrived on THIS pass (the resolves above) must
        // reach cards whose container was loaded on an earlier one — PostTipDisplay.Bind and
        // SoldPostTeaser.Bind snapshot, and a non-recycling container is loaded once. See
        // RefreshCardPayments.
        RefreshCardPayments();
#endif   // PAYMENTS
    }

    /// <summary>Resolves <paramref name="hash"/>'s `x-c2pa` hint (cache-shared with the
    /// list-card image's own load, see the trigger in <see cref="Sync"/>) and
    /// records it on <paramref name="item"/>. Safe if the row has since scrolled out of
    /// <see cref="Posts"/> or been replaced by a content-changed reconcile: the result
    /// lands on the SPECIFIC captured instance, never a shared control, so a stale
    /// completion just updates an orphaned view-model with no listener — the class of
    /// bug <c>ImageHashBind</c>'s request-version guard exists for does not arise here.</summary>
    private static async Task TriggerC2paCheckAsync(FeedPostItem item, BlobImageLoader loader, string hash)
    {
        var (_, hasC2pa) = await loader.LoadWithC2paAsync(hash, item.BlobHome);
        item.SetHasC2pa(hasC2pa);
    }

    internal async void LikeButton_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel is null || sender is not Button btn || btn.Tag is not string postId) return;
        await RunVerbAsync("like", () => ViewModel!.LikeAsync(postId));
    }

    /// <summary>A <c>post-reaction-pill</c> tap — the shared <c>FeedManager::react</c> toggle
    /// (composes the viewer's reaction or takes it back); a refusal (a bridged target) lands on
    /// <c>error-message</c> through <see cref="RunVerbAsync"/>. The pill's data context is its
    /// <see cref="PostReactionPill"/> in both the card template and the detail dialog.</summary>
    internal async void ReactionPill_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel is null || sender is not FrameworkElement { DataContext: PostReactionPill pill }) return;
        await RunVerbAsync("react", () => ViewModel!.ReactAsync(pill.PostId, pill.Emoji));
    }

    /// <summary>The detail dialog's pill row: the same pills as the card, built imperatively
    /// (the dialog has no DataTemplate), each carrying its <see cref="PostReactionPill"/> as
    /// DataContext so <see cref="ReactionPill_Click"/> serves both.</summary>
    internal StackPanel BuildDetailReactionPills(FeedPostItem item)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        foreach (var pill in item.ReactionPills)
        {
            var btn = new Button
            {
                Padding = new Thickness(8, 2, 8, 2),
                FontSize = 12,
                Content = pill.Label,
                DataContext = pill,
                Style = PostCardBind.LikedToButtonStyle(pill.Own),
            };
            AutomationProperties.SetAutomationId(btn, Ids.PostReactionPill);
            AutomationProperties.SetName(btn, pill.Label);
            AutomationProperties.SetHelpText(btn, pill.State);
            btn.Click += ReactionPill_Click;
            row.Children.Add(btn);
        }
        return row;
    }

    /// <summary>Run one interaction-bar verb and put a rejection on <c>error-message</c>
    /// rather than let it escape the <c>async void</c> click handler unseen:
    /// <see cref="FeedViewModel.VerbErrorCopy"/> reads a stated refusal in the user's
    /// language (words under a restricted post — <c>ui/feed.md</c> § Encryption at rest,
    /// ruling 6) and keeps any other failure's own text. A verb that lands clears a
    /// previous verb's error, as web's does.</summary>
    internal async Task RunVerbAsync(string verb, Func<Task> run)
    {
        try
        {
            await run();
            _page.ClearPageError();
        }
        catch (Exception ex)
        {
            ShellLog.Error(nameof(PostCardHost), $"feed {verb} failed: {ex.Message}");
            _page.ReportPageError(FeedViewModel.VerbErrorCopy(ex));
        }
    }

    /// <summary>Opens the reply compose surface (<c>feed-reply-dialog</c>) and, on submit,
    /// composes a real reply through <c>FeedViewModel.ReplyAsync</c> (<c>FfiFeedManager::reply</c>)
    /// — NOT <c>InteractAsync(id, "reply", body)</c>, whose native arm discards the typed text
    /// outright (feed.md § Interaction bar; § Implementation status today). Submit/cancel live as
    /// real <c>Button</c>s in the dialog's <c>Content</c>, mirroring the feed page's <c>CreateFeed_Click</c>
    /// create/cancel row: a <c>ContentDialog</c>'s template Primary/Close buttons cannot carry a
    /// custom <c>AutomationId</c> (see the comment on that row), and <c>feed-reply-submit-button</c>
    /// needs one. No-op on blank/whitespace-only text (mirrors linux's <c>build_reply_dialog</c>
    /// guard) — unlike quote, an empty reply is meaningless.</summary>
    internal async void ReplyButton_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel is null || sender is not Button btn || btn.Tag is not string postId) return;

        var textBox = new TextBox
        {
            PlaceholderText = S.Get("feed/post/write_reply"),
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            Height = 100,
        };
        AutomationProperties.SetAutomationId(textBox, Ids.FeedReplyTextField);

        ContentDialog? replyDialog = null;
        bool shouldReply = false;

        var buttonRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            HorizontalAlignment = HorizontalAlignment.Right,
        };
        var cancelBtn = new Button { Content = S.Get("common/cancel") };
        cancelBtn.Click += (s, _) => replyDialog?.Hide();
        var submitBtn = new Button
        {
            Content = S.Get("common/reply"),
            Style = (Style)Application.Current.Resources["AccentButtonStyle"],
        };
        AutomationProperties.SetAutomationId(submitBtn, Ids.FeedReplySubmitButton);
        submitBtn.Click += (s, _) =>
        {
            if (string.IsNullOrWhiteSpace(textBox.Text)) return;
            shouldReply = true;
            replyDialog?.Hide();
        };
        buttonRow.Children.Add(cancelBtn);
        buttonRow.Children.Add(submitBtn);

        var panel = new StackPanel { Spacing = 8 };

        // Under a restricted target the dialog STATES where the reply goes —
        // `feed-reply-audience`, the manager's own per-post answer, never decided here —
        // and offers the explicit-public answer (`feed-reply-public-confirm`) only under
        // *public by confirmation* (restricted-posts.md § Encryption at rest → *Ruling 5's
        // build — the shape*, (d) + (e)). A public target paints neither. The checkbox is
        // born unchecked at every open and never remembered.
        var target = ViewModel.FindPost(postId);
        var audience = target?.replyAudience;
        CheckBox? publicConfirm = null;
        if (target is not null && audience is { } a)
        {
            var audienceText = a switch
            {
                ReplyAudience.SealedToRoom => S.Format("feed/post/reply_audience_room", target.roomLabel ?? ""),
                ReplyAudience.SealedToTier => S.Format("feed/post/reply_audience_tier", target.gatedTier ?? ""),
                _ => S.Get("feed/post/reply_audience_public"),
            };
            var audienceLabel = new TextBlock { Text = audienceText, TextWrapping = TextWrapping.Wrap };
            AutomationProperties.SetAutomationId(audienceLabel, Ids.FeedReplyAudience);
            panel.Children.Add(audienceLabel);
        }
        panel.Children.Add(textBox);
        if (audience == ReplyAudience.PublicByConfirmation)
        {
            publicConfirm = new CheckBox
            {
                Content = S.Get("feed/post/reply_public_confirm"),
                IsChecked = false,
            };
            AutomationProperties.SetAutomationId(publicConfirm, Ids.FeedReplyPublicConfirm);
            panel.Children.Add(publicConfirm);
        }
        panel.Children.Add(buttonRow);

        var dialog = new ContentDialog
        {
            Title = S.Get("common/reply"),
            Content = panel,
            XamlRoot = _page.XamlRoot,
        };
        AutomationProperties.SetAutomationId(dialog, Ids.FeedReplyDialog);
        replyDialog = dialog;

        await Controls.Dialogs.ShowAsync(dialog);

        if (shouldReply)
        {
            var body = textBox.Text.Trim();
            // The checkbox is only ever present (hence only ever checkable) under
            // *public by confirmation*; the send path (prepare → upload → submit /
            // confirmed / plain) lives in FeedViewModel.SendReplyAsync.
            var confirmed = publicConfirm?.IsChecked == true;
            await RunVerbAsync("reply", () => ViewModel!.SendReplyAsync(App.CurrentNest, postId, body, confirmed));
        }
    }

    /// <summary>Repost / un-repost toggle — through <c>FeedViewModel.RepostAsync</c>
    /// (<c>FfiFeedManager::repost</c>), NOT the raw <c>InteractAsync(id, "repost")</c>: that
    /// one-way call creates no repost post on a native target and can never take one back
    /// (feed.md § Interaction bar → Repost, ratified 2026-08-10).
    /// Matches <see cref="LikeButton_Click"/>'s shape.</summary>
    internal async void RepostButton_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel is null || sender is not Button btn || btn.Tag is not string postId) return;
        await RunVerbAsync("repost", () => ViewModel!.RepostAsync(postId));
    }

    /// <summary>Quote-repost — a repost WITH commentary (<c>Reference::Quote</c>) — through
    /// <c>FeedViewModel.QuoteAsync</c> (<c>FfiFeedManager::quote</c>), NOT
    /// <c>InteractAsync(id, "quote")</c>: the nest's native arm never composes a post from
    /// that door (feed.md § Interaction bar; § Implementation status today). Fires with an
    /// empty body — the ratified direct-tap shape (matches tui/linux/web/android/macos/ios);
    /// a commentary composer is a separate fleet-wide follow-on. The nest bumps the target's
    /// <c>quote_count</c> when the composed quoting post lands referencing it.</summary>
    internal async void QuoteButton_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel is null || sender is not Button btn || btn.Tag is not string postId) return;
        await RunVerbAsync("quote", () => ViewModel!.QuoteAsync(postId, ""));
    }

    /// <summary>Dispatch <c>load-remote-content-button</c> to the manager (D3): the manager
    /// flips the in-memory reveal set for this post and re-emits; the next <c>Snapshot()</c>
    /// projects <c>RemoteImage.revealed=true</c> → Refresh rebuilds the row →
    /// FeedPostBodyBind repaints with fetched images (render-model.md § D3;
    /// html-mail.md § Security &amp; privacy). NO local flip.</summary>
    internal void LoadRemoteContent_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel is not null && sender is FrameworkElement fe && fe.DataContext is FeedPostItem item)
            ViewModel.RevealRemoteImages(item.PostId);
    }

    /// <summary>Per-card <c>TipDisplayHost</c> fill, driven by the host's own
    /// <c>Loaded</c> (see the ContentControl's comment in Controls/PostCardTemplates.xaml for why that
    /// and not <c>ContainerContentChanging</c>).
    ///
    /// <para>Declared unconditionally, body under <c>PAYMENTS</c>: a DataTemplate event
    /// handler is resolved against this class at XAML-compile time and XAML carries no
    /// <c>#if</c>, so hiding the whole method behind the excision would break the
    /// store-safe build that the excision exists to produce.</para></summary>
    internal void TipDisplayHost_Loaded(object sender, RoutedEventArgs e)
    {
#if PAYMENTS
        if (sender is ContentControl host && host.DataContext is FeedPostItem post)
            BindTipDisplay(host, post);
#endif   // PAYMENTS
    }

    /// <summary>Per-card <c>SoldPostTeaserHost</c> fill — the sold-post buyer teaser
    /// (gap (2c), monetization.md § Per-post pay-to-unlock), <see cref="TipDisplayHost_Loaded"/>'s
    /// shape and for the same reasons.</summary>
    internal void SoldPostTeaserHost_Loaded(object sender, RoutedEventArgs e)
    {
#if PAYMENTS
        if (sender is ContentControl host && host.DataContext is FeedPostItem post)
            BindSoldPostTeaser(host, post);
#endif   // PAYMENTS
    }

#if PAYMENTS
    /// <summary>Fill (or first-create) one realized post-card's <c>SoldPostTeaserHost</c>
    /// with a bound <see cref="Views.Payments.SoldPostTeaser"/> — <see cref="BindTipDisplay"/>'s
    /// twin. Idempotent: the control is created once per host and re-bound thereafter.</summary>
    private void BindSoldPostTeaser(ContentControl host, FeedPostItem post)
    {
        if (host.Content is not Views.Payments.SoldPostTeaser teaser)
        {
            teaser = new Views.Payments.SoldPostTeaser();
            teaser.PaymentLinkRequested += (_, item) => OpenUnlockOfferPaymentLink(item);
            teaser.BuyRequested += (_, item) => BuyUnlockOffer(item);
            host.Content = teaser;
        }
        teaser.Bind(post);
    }

    /// <summary>Open the sold-post offer's external payment_url (the teaser's payment
    /// link) — mirrors <c>ProfilePage.OpenOfferPaymentLink</c>, plus the shared
    /// <c>uniffi.fauna_core.FaunaCoreMethods.IsSafePaymentUrl</c> guard (F-CL2
    /// anti-phishing-redirect class — the nest/author-supplied url is
    /// untrusted): a non-https url shows <c>subscriptions/unsafe_payment_url</c> on
    /// `error-message` instead of launching.</summary>
    private void OpenUnlockOfferPaymentLink(FeedPostItem post)
    {
        if (string.IsNullOrEmpty(post.UnlockOfferPaymentUrl)) return;
        if (!uniffi.fauna_core.FaunaCoreMethods.IsSafePaymentUrl(post.UnlockOfferPaymentUrl))
        {
            var msg = S.Get("subscriptions/unsafe_payment_url");
            _page.ShowError(msg);
            return;
        }
        FaunaApp.Services.UrlOpener.Open(post.UnlockOfferPaymentUrl, "Feed");
    }

    /// <summary>Buy the sold post's unlock tier off the resolved teaser offer (the teaser's
    /// buy button) — the existing subscribe flow against the resolved offer's tier, queued
    /// pending the author's own §2 approve. A successful call mutates the manager's own
    /// snapshot + notifies (mirrors <see cref="DispatchTrainAsync"/>'s error-surfacing
    /// convention).</summary>
    private async void BuyUnlockOffer(FeedPostItem post)
    {
        if (ViewModel is null) return;
        try
        {
            await ViewModel.BuyUnlockOfferAsync(post.PostId);
        }
        catch (Exception ex)
        {
            var msg = S.Format("feed/error_buy_unlock", ex.Message);
            _page.ShowError(msg);
        }
    }

    /// <summary>Fill both per-card payments hosts of one realized card's template root —
    /// what <see cref="RefreshCardPayments"/> and the dormant virtualizing path share.</summary>
    private void BindCardPayments(FrameworkElement root, FeedPostItem post)
    {
        if (root.FindName("TipDisplayHost") is ContentControl tipHost) BindTipDisplay(tipHost, post);
        if (root.FindName("SoldPostTeaserHost") is ContentControl teaserHost) BindSoldPostTeaser(teaserHost, post);
    }

    /// <summary>Fill (or first-create) one realized post-card's <c>TipDisplayHost</c>
    /// with a bound <see cref="Views.Payments.PostTipDisplay"/> — the per-card twin of
    /// <c>ProfilePage.Page_Loaded</c>'s single <c>PaymentsSectionsHost</c> fill, done
    /// per container because the plane is per-card rather than a single page section.
    /// Idempotent: the control is created once per host and re-bound thereafter.</summary>
    private void BindTipDisplay(ContentControl host, FeedPostItem post)
    {
        if (host.Content is not Views.Payments.PostTipDisplay display)
        {
            display = new Views.Payments.PostTipDisplay();
            display.OpenTipListRequested += (_, item) => OpenTipList(item);
            host.Content = display;
        }
        display.Bind(post);
    }

    /// <summary>Re-bind every ALREADY-REALIZED card's tip display and sold-post teaser,
    /// after a snapshot pass has reconciled <see cref="Posts"/>.
    ///
    /// <para>This is the half <c>Loaded</c> cannot cover. <see cref="Views.Payments.PostTipDisplay.Bind"/>
    /// and <see cref="Views.Payments.SoldPostTeaser.Bind"/> SNAPSHOT their item's values
    /// into <c>Text</c>/<c>Visibility</c> rather than binding them, and a container on a
    /// non-recycling panel is loaded exactly once — so a tip state or an unlock offer that
    /// arrives LATER (the `ResolvePostTipsAsync` / `ResolvePostUnlockOfferAsync` round
    /// trips in <see cref="Sync"/>, which are the whole live path) would paint nothing
    /// without a pass like this. Containers not yet generated are simply skipped; their own
    /// <c>Loaded</c> binds them with the same fresh item.</para></summary>
    private void RefreshCardPayments()
    {
        foreach (var post in Posts)
        {
            if (_list.ContainerFromItem(post) is not ContentControl container) continue;
            if (container.ContentTemplateRoot is not FrameworkElement root) continue;
            BindCardPayments(root, post);
        }
    }

    /// <summary>The virtualizing-panel path, kept deliberately and WIRED BUT DORMANT:
    /// with this list's non-virtualizing <c>ItemsPanel</c> override it does not fire at
    /// all (that is the defect), and it would be the right seam again the moment
    /// the panel becomes virtualizing — recycling reuses a container without re-raising
    /// <c>Loaded</c>. Left in place rather than deleted so a future panel change does not
    /// silently lose the re-bind; do not read its existence as evidence that it runs.</summary>
    private void List_ContainerContentChanging(ListViewBase sender, ContainerContentChangingEventArgs args)
    {
        if (args.Item is not FeedPostItem post) return;
        if (args.ItemContainer?.ContentTemplateRoot is not FrameworkElement root) return;
        BindCardPayments(root, post);
    }

    /// <summary>Open the tip attribution window (monetization.md § Tips) for one
    /// post — flat, not scoped: exactly one open at a time. An inline Border +
    /// Visibility toggle, NOT a <c>ContentDialog</c> — the AdminDnsPage RenameSheet
    /// precedent ("a modal ContentDialog does NOT surface ... to the FlaUI
    /// is_visible tree") held here too: a ContentDialog shape measured
    /// 0x0/isOffscreen the moment this row's own e2e checked <c>is_visible</c> right
    /// after open, with no wait. Shared by the list card's
    /// <c>post-tip-list-button</c> (via
    /// <see cref="Views.Payments.PostTipDisplay.OpenTipListRequested"/>) and the
    /// detail dialog's own tip row. The item's precomputed
    /// <see cref="FeedPostItem.TipListTitle"/> / <see cref="FeedPostItem.TipSenders"/>
    /// are set once at construction (Tips is an immutable snapshot field), so this
    /// just paints them — no fresh read.</summary>
    internal void OpenTipList(FeedPostItem post) => _tipListDialog?.Show(post.TipListTitle, post.TipSenders);
#endif

    // ── Trained-topic training verbs (post-card ⋯ overflow;
    //    topic-factors.md § Training signals) ─────────────────────────────

    /// <summary>Open post's ⋯ overflow (<c>feed-post-actions-button</c> →
    /// <c>feed-post-actions-menu</c>): the two training verbs, own-post delete, and
    /// the own-post web-publishing verbs (web-content-hosting.md § Published-post
    /// management), checked against the post's current example marker for the
    /// feed's in-context training target (<c>FeedManager::train_target_factor</c> —
    /// a feed whose composition singles out one trained factor trains it directly;
    /// otherwise the verb opens the factor-target sheet). Mirrors
    /// <c>DmMessageBubble.ShowDeleteConfirm</c>'s explicit <c>ShowAt</c> (not a
    /// pre-assigned <c>Button.Flyout</c> — this menu's content depends on state
    /// read fresh at open time).
    ///
    /// <c>async</c> so the web verbs' origin (whether the actor has a serving
    /// address to build copy links on) is resolved BEFORE the menu paints — tui's
    /// trap #2: painting a *disabled* copy verb off unread state tells a creator
    /// with a working address that they have none, and a <c>MenuFlyoutItem</c>'s
    /// state can't be live-patched once shown the way a data-bound row can.</summary>
    internal async void PostActionsButton_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel is null || sender is not Button { Tag: FeedPostItem post } btn) return;
        var manager = ViewModel.Manager;
        var targetFactor = manager.TrainTargetFactor();

        // Own-post gates (feed.md § State & data shape → Post deletion, IDs
        // user-approved 2026-07-16; web-content-hosting.md § Published-post
        // management) — gated to the local actor's own post, the same shape
        // linux (client.actor_id() == post.author) and web (isOwn derived from
        // $identity) already ship. FeedPostItem carries only the author's hex
        // actor id (no is_own flag from the FFI snapshot), so the comparison
        // happens here against the page's own ICryptoService.
        var ownActorHex = Crypto?.HasKey == true ? Crypto.ActorIdHex : null;
        var isOwn = ownActorHex is not null && post.AuthorHex == ownActorHex;

        // Resolve the site-link origin ONLY when this own post could show a copy
        // verb (already published — WebSlug set). An unpublished post's sole verb
        // (Publish to web) needs no origin, so this round trip is skipped for the
        // common case of opening the menu on an ordinary own post.
        uniffi.fauna_client_web.SiteLinkView? siteLink = null;
        if (isOwn && post.WebSlug is { Length: > 0 } && Rpc is not null && Account is not null)
        {
            try
            {
                var web = await GetWebClientAsync();
                var handle = Account.Handle;
                var resolved = await WebOrigin.ResolveAsync(web, handle);
                siteLink = resolved.SiteLink;
            }
            catch (Exception)
            {
                siteLink = null; // treat a failed resolve as "no origin" — dead, not silently live
            }
        }

        var menu = new MenuFlyout();
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(menu, Ids.FeedPostActionsMenu);
        // Copy verbs must survive their own Click without the flyout auto-dismissing
        // (a MenuFlyoutItem click always closes its parent MenuFlyout — see
        // ShowDeletePostConfirm's doc comment below) so the e2e can read the
        // `copied` attr right after clicking, the same "menu stays open after a
        // copy" shape web's PostCard.svelte uses (only publish/unpublish close it).
        var suppressClose = false;
        menu.Closing += (_, args) =>
        {
            if (!suppressClose) return;
            args.Cancel = true;
            suppressClose = false;
        };

        void AddVerb(TrainVerb verb, string labelKey, string id)
        {
            var marked = targetFactor is { } f && manager.ExampleLabelFor(post.PostId, f) == verb;
            var item = new MenuFlyoutItem { Text = S.Get(labelKey) };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(item, id);
            // The checked marker rides HelpText (get_attr(item, "state") maps any
            // non-disabled/non-name attr to HelpText — reference_windows_flaui_state_attr_helptext).
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(item, marked ? "on" : "off");
            item.Click += async (_, _) =>
            {
                if (targetFactor is { } factor)
                    await DispatchTrainAsync(post.PostId, factor, verb, marked);
                else
                    OpenTrainTargetSheet(btn, post.PostId, verb);
            };
            menu.Items.Add(item);
        }

        AddVerb(TrainVerb.MoreLikeThis, "feed/more_like_this", "feed-post-more-like-this");
        AddVerb(TrainVerb.LessLikeThis, "feed/less_like_this", "feed-post-less-like-this");

        // The report verb (moderation.md § User-initiated reporting → App surface),
        // gated !is_own: nobody reports their own post. The target is built by the
        // shared report_post_target, which carries the sealed rule once (a gated or
        // room post is sealed — the nest holds no readable bytes for it).
        if (!isOwn && Rpc is not null)
        {
            var report = new MenuFlyoutItem { Text = S.Get("feed/report_post") };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(report, Ids.FeedPostReportButton);
            report.Click += async (_, _) => await OpenReportSheetAsync(post);
            menu.Items.Add(report);
        }

        if (isOwn)
        {
            var del = new MenuFlyoutItem { Text = S.Get("feed/delete_post") };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(del, Ids.FeedPostDeleteButton);
            del.Click += (_, _) => ShowDeletePostConfirm(btn, post.PostId);
            menu.Items.Add(del);

            BuildWebPublishVerbs(menu, post, siteLink, () => suppressClose = true);
        }

        menu.ShowAt(btn);
    }

    /// <summary>Open the shared report sheet on <paramref name="post"/> (the ⋯ menu's
    /// <c>feed-post-report-button</c>). A landed report is acknowledged OUTSIDE the
    /// closed sheet (<c>report-status</c>); its reporter-side hide has already been
    /// stored by the sheet, so the re-sync below makes the card read "You reported
    /// this" at once. A failed send keeps the sheet open and reports on
    /// <c>error-message</c>; a failed block/hide lands there BESIDE the
    /// acknowledgement (moderation.md § Corollary — never silent).</summary>
    private async Task OpenReportSheetAsync(FeedPostItem post)
    {
        if (Rpc is null) return;
        _page.ShowReportStatus(null);
        var target = FaunaFfiMethods.ReportPostTarget(
            post.PostId, post.AuthorHex, post.BodyText,
            post.GatedTier is { Length: > 0 } || !string.IsNullOrEmpty(post.RoomLabel));
        var outcome = await Controls.ReportSheetDialog.OpenAsync(
            _page.XamlRoot, Rpc, target, _page.ReportPageError);
        if (outcome is null) return;
        _page.ShowReportStatus(outcome.Acknowledgement ?? "");
        if (outcome.FollowUpError is { } followUp) _page.ReportPageError(followUp);
        // The hide list changed: re-read the rows (each verdict is read at construction).
        _page.RefreshCards();
    }

    /// <summary>The own-post web-publishing verbs (web-content-hosting.md
    /// § Published-post management; presence rules ui/feed.md § User actions).
    ///
    /// Everything here is state-derived off the post the snapshot already holds —
    /// <see cref="FeedPostItem.WebSlug"/> / <see cref="FeedPostItem.GatedTier"/> —
    /// never a per-row query. <paramref name="siteLink"/> is the caller's already-
    /// resolved origin (or null — no origin, or not an own published post, in
    /// which case the copy verbs are simply not offered a live link).
    ///
    /// <paramref name="markCopyPending"/> is called by a copy verb's Click just
    /// before it writes to the clipboard, so the caller's <c>Closing</c> handler
    /// keeps the menu open long enough for the `copied` attr to be read.</summary>
    private void BuildWebPublishVerbs(
        MenuFlyout menu, FeedPostItem post, uniffi.fauna_client_web.SiteLinkView? siteLink,
        Action markCopyPending)
    {
        if (post.WebSlug is not { Length: > 0 } slug)
        {
            // Unpublished: one verb, no link affordances for a page that does not
            // exist. A default slug is the nest's to mint, so this needs no origin.
            var publish = new MenuFlyoutItem { Text = S.Get("web_publish/publish_to_web") };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(publish, Ids.FeedPostPublishWebButton);
            publish.Click += async (_, _) => await DispatchWebPublishAsync(post.PostId);
            menu.Items.Add(publish);
            return;
        }

        var origin = siteLink?.@origin;
        // Said once for the menu rather than per verb, and only when the paywall
        // affordance is actually present below (monetization.md § Pillar 2 →
        // Creator comp-link surface: the ratified ~10-minute validity).
        if (post.GatedTier is { Length: > 0 } && origin is not null)
            menu.Items.Add(new MenuFlyoutItem { Text = S.Get("web_publish/paywall_link_note"), IsEnabled = false });
        // Why the copy verbs below are dead, in the user's own terms — the ⋯ menu
        // cannot say "the toggle above" (that control is on another page).
        if (origin is null)
            menu.Items.Add(new MenuFlyoutItem { Text = S.Get("web_publish/menu_no_link_reason"), IsEnabled = false });

        var copyWeb = new MenuFlyoutItem
        {
            Text = S.Get("web_publish/copy_web_link"),
            IsEnabled = origin is not null,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(copyWeb, Ids.FeedPostCopyWebLinkButton);
        copyWeb.Click += (_, _) =>
        {
            if (origin is not { } o) return;
            markCopyPending();
            var url = FaunaFfiMethods.WebPostPageUrl(o, slug);
            FaunaApp.Helpers.ClipboardHelper.CopyText(url);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(copyWeb, url);
        };
        menu.Items.Add(copyWeb);

        // Gated rows only: an ungated post has no paywalled body, so the mint
        // would hand out a token for nothing.
        if (post.GatedTier is { Length: > 0 })
        {
            var copyPaywall = new MenuFlyoutItem
            {
                Text = S.Get("web_publish/copy_paywall_link"),
                IsEnabled = origin is not null,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(copyPaywall, Ids.FeedPostCopyPaywallLinkButton);
            copyPaywall.Click += async (_, _) =>
            {
                if (origin is not { } o) return;
                markCopyPending();
                await DispatchCopyPaywallLinkAsync(copyPaywall, post.PostId, slug, o);
            };
            menu.Items.Add(copyPaywall);
        }

        var unpublish = new MenuFlyoutItem { Text = S.Get("web_publish/unpublish") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(unpublish, Ids.FeedPostUnpublishWebButton);
        unpublish.Click += async (_, _) => await DispatchWebUnpublishAsync(post.PostId);
        menu.Items.Add(unpublish);
    }

    /// <summary>Lazily build the shared <c>FfiWebClient</c> over the session's
    /// shared, auto-reconnecting WS-RPC connection (mirrors
    /// <c>SettingsWebPage.EnsureViewModelAsync</c> — never a per-open one-shot
    /// connection).</summary>
    private async Task<uniffi.fauna_ffi.FfiWebClient> GetWebClientAsync()
    {
        if (_webClient is not null) return _webClient;
        if (Rpc is null) throw new InvalidOperationException("no RPC client");
        _webClient = await Rpc.BuildWebClientAsync();
        return _webClient;
    }

    /// <summary>Publish a post as a web page from the ⋯ menu (the nest mints the
    /// default slug — this menu offers no client-chosen slug). A successful call
    /// re-reads the current feed so the card's WebSlug (which drives the whole
    /// verb family) repaints from the nest's own answer, same as the section's own
    /// non-optimistic pattern.</summary>
    private async Task DispatchWebPublishAsync(string postIdHex)
    {
        if (ViewModel is null) return;
        try
        {
            var web = await GetWebClientAsync();
            await web.PublishSet(Convert.FromHexString(postIdHex), null);
            await ViewModel.Manager.RefreshCurrentFeed();
        }
        catch (Exception ex)
        {
            var msg = S.Format("web_publish/error_publish", ex.Message);
            _page.ShowError(msg);
        }
    }

    /// <summary>Take a published page down from the ⋯ menu. One tap, no confirm
    /// step — unlike delete, a takedown is idempotent and reversible.</summary>
    private async Task DispatchWebUnpublishAsync(string postIdHex)
    {
        if (ViewModel is null) return;
        try
        {
            var web = await GetWebClientAsync();
            await web.PublishUnset(Convert.FromHexString(postIdHex));
            await ViewModel.Manager.RefreshCurrentFeed();
        }
        catch (Exception ex)
        {
            var msg = S.Format("web_publish/error_unpublish", ex.Message);
            _page.ShowError(msg);
        }
    }

    /// <summary>Mint + copy a short-lived full-access paywall link. A fresh mint
    /// per click: the token is short-lived by ratified design and re-minting is
    /// free, so re-copying always yields a link that works from now rather than a
    /// cached one that already expired.</summary>
    private async Task DispatchCopyPaywallLinkAsync(MenuFlyoutItem button, string postIdHex, string slug, string origin)
    {
        try
        {
            var web = await GetWebClientAsync();
            var minted = await web.PaywallMintToken(new uniffi.fauna_client_web.PaywallTarget.PostSlug(slug));
            var url = FaunaFfiMethods.WebTokenedUrl(origin, minted.@path, minted.@token);
            FaunaApp.Helpers.ClipboardHelper.CopyText(url);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(button, url);
        }
        catch (Exception ex)
        {
            var msg = S.Format("web_publish/error_paywall_link", ex.Message);
            _page.ShowError(msg);
        }
    }

    /// <summary>Open the delete-confirm step (<c>feed-post-delete-confirm-button</c>) as a
    /// sub-flyout anchored to the ⋯ button — mirrors
    /// <c>DmMessageBubble.ShowDeleteConfirm</c>'s established reveal-in-flyout shape
    /// verbatim (title + a single always-enabled confirm Button in its own
    /// <c>Flyout</c>): a <c>MenuFlyoutItem</c> click always closes its parent
    /// <c>MenuFlyout</c>, so the confirm step needs its own flyout rather than a
    /// second item toggled visible inside <c>menu</c>.</summary>
    private void ShowDeletePostConfirm(FrameworkElement anchor, string postId)
    {
        var panel = new StackPanel { Orientation = Orientation.Vertical, Spacing = 8, MinWidth = 180 };
        panel.Children.Add(new TextBlock
        {
            Text = S.Get("feed/delete_post_confirm_title"),
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
        });
        var confirm = new Button
        {
            Content = S.Get("feed/delete_post_confirm"),
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(confirm, Ids.FeedPostDeleteConfirmButton);
        confirm.Click += async (_, _) =>
        {
            _postDeleteConfirmFlyout?.Hide();
            await DispatchDeletePostAsync(postId);
        };
        panel.Children.Add(confirm);

        _postDeleteConfirmFlyout = new Flyout { Content = panel };
        _postDeleteConfirmFlyout.ShowAt(anchor);
    }

    private Flyout? _postDeleteConfirmFlyout;

    /// <summary>Delete an own post via the shared <c>FeedManager::delete_post</c>
    /// (mirrors <see cref="DispatchTrainAsync"/>'s error-surfacing convention). A
    /// successful call mutates the manager's own snapshot + notifies, so
    /// the page's repaint drops the row via the normal observer path — no direct
    /// re-render call here (same as the training verbs above).</summary>
    private async Task DispatchDeletePostAsync(string postId)
    {
        if (ViewModel is null) return;
        try
        {
            await ViewModel.Manager.DeletePost(postId);
        }
        catch (Exception ex)
        {
            var msg = S.Format("feed/error_delete", ex.Message);
            _page.ShowError(msg);
        }
    }

    /// <summary>Run a training gesture: the marked verb again ⇒ un-train (the
    /// inverse delta), anything else ⇒ train (the manager applies forward/flip
    /// semantics — no UI-side duplicate guard, <c>TrainResult.DuplicateSignal</c>
    /// writes nothing). A successful call mutates the manager's own snapshot +
    /// notifies, so the page's repaint rebuilds the row via the normal observer
    /// path — no direct re-render call here. A refusal needs no bridge either: the
    /// manager reports it as its own page error (<c>feed.error_train</c> on the
    /// snapshot's error) and notifies, so the page's repaint paints it on
    /// <c>error-message</c> — the one slot, which a paint from this catch would only
    /// race or shadow (linux's <c>dispatch_train</c>, tui's train arm).</summary>
    private async Task DispatchTrainAsync(string postId, string factor, TrainVerb verb, bool alreadyMarked)
    {
        if (ViewModel is null) return;
        try
        {
            if (alreadyMarked) await ViewModel.Manager.UntrainPost(postId, factor);
            else await ViewModel.Manager.TrainPost(postId, factor, verb);
        }
        catch (Exception ex)
        {
            ShellLog.Warn(nameof(PostCardHost), $"feed train refused: {ex.Message}");
        }
    }

    /// <summary>The factor-target sheet (<c>feed-post-train-target-sheet</c>):
    /// shown when the current feed's composition does not single out one trained
    /// topic. A sheet of tappable rows (ratified 2026-10-16, every app): one
    /// <c>feed-post-train-target-item</c> button per trained topic in registry
    /// order (text = the user's name, <c>factor</c> attr = the key's hex on
    /// HelpText — the <c>personalization-trained-factor-item</c> convention), one
    /// tap trains with the verb that opened the sheet (the marked verb again
    /// un-marks), then closes the sheet. "New trained topic…"
    /// (<c>feed-post-train-target-new-button</c>) is always last and leaves for
    /// the Personalization home rather than duplicating the create form inline.
    /// The flyout opens once the sealed-registry read resolves (linux's shape), so
    /// the rows are present whenever the sheet is.</summary>
    private async void OpenTrainTargetSheet(FrameworkElement anchor, string postId, TrainVerb verb)
    {
        var panel = new StackPanel { Orientation = Orientation.Vertical, Spacing = 8, MinWidth = 220 };
        // A bare AutomationId container is pruned from UIA; the Name keeps it findable.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(panel, Ids.FeedPostTrainTargetSheet);
        var title = S.Get("feed/train_target_title");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(panel, title);
        panel.Children.Add(new TextBlock
        {
            Text = title,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
        });

        var flyout = new Flyout { Content = panel };

        IReadOnlyList<FfiTrainedTopicRow> rows = Array.Empty<FfiTrainedTopicRow>();
        if (Rpc is not null)
        {
            try { rows = await Rpc.TrainedTopicsListAsync(); }
            catch (Exception ex)
            {
                ShellLog.Warn(nameof(PostCardHost), $"trained-topics fetch for target sheet failed: {ex.Message}");
            }
        }

        foreach (var row in rows)
        {
            if (row.factorKey is not { } key) continue;
            var hex = key.StartsWith("topic:", StringComparison.Ordinal) ? key["topic:".Length..] : key;
            var item = new Button
            {
                Content = row.name,
                HorizontalAlignment = HorizontalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Left,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(item, Ids.FeedPostTrainTargetItem);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, row.name);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(item, hex);
            item.Click += async (_, _) =>
            {
                flyout.Hide();
                var marked = ViewModel?.Manager.ExampleLabelFor(postId, key) == verb;
                await DispatchTrainAsync(postId, key, verb, marked);
            };
            panel.Children.Add(item);
        }

        var createButton = new Button
        {
            Content = S.Get("personalization/trained_factor_create"),
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Left,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(createButton, Ids.FeedPostTrainTargetNewButton);
        createButton.Click += (_, _) =>
        {
            flyout.Hide();
            MainPage.Current?.NavigateToSettingsSubPage(SettingsNavigation.Personalization);
        };
        panel.Children.Add(createButton);

        flyout.ShowAt(anchor);
    }

    /// <summary>Open <paramref name="authorHex"/>'s profile with the post's
    /// <paramref name="home"/> carried as the actor-home hint. An id naming the viewer
    /// normalizes to SELF through the shared <c>profile_nav_target</c>.</summary>
    internal void OpenAuthorProfile(string authorHex, string? home)
    {
        var selfHex = Crypto?.HasKey == true ? Crypto.ActorIdHex : null;
        var target = uniffi.fauna_ffi.FaunaFfiMethods.ProfileNavTarget(authorHex, selfHex);
        App.PendingProfileTarget = target;
        App.PendingProfileHome = target is null ? null : home;
        MainPage.Current?.NavigateToView("profile");
    }

    /// <summary><c>post-author</c> on a card — open the author's profile.</summary>
    internal void PostAuthor_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: FeedPostItem item })
            OpenAuthorProfile(item.AuthorHex, item.HomeNestUrl);
    }

    // Opens the post_detail for the clicked card (ui.yaml feed transition
    // `click post-card → post_detail`). The post-card DataTemplate root is a
    // transparent Button (the event-card/addressbook-item/media-item idiom): a bare
    // Grid/StackPanel root exposes no UIA invocation pattern, so the FlaUI bridge's
    // physical-click fallback lands but never fires the open (reference_winui_clickable
    // _row_needs_button_root). Unlike event-card, the card's own interactive children
    // (feed-post-actions-button, load-remote-content-button, the like/reply/repost/quote
    // bar, feed-post-muted-reveal-button) stay NESTED inside this Button rather than
    // moved out as siblings — the feed e2e scopes several of them under `post-card[i]`
    // (feed.py reveal_remote_content / open_post_actions; test_feed_link_preview), and
    // WalkScope confines a scoped lookup to the post-card element's own subtree
    // (flaui-bridge/ElementFinder.cs). Each nested child keeps its own InvokePattern, so
    // FlaUI drives it directly; the outer Button's Invoke opens the detail without
    // double-firing (Invoke is direct, not pointer-routed). The lists carry no
    // IsItemClickEnabled, so this Click is the sole open path (no ItemClick double-open).
    internal async void PostCard_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: FeedPostItem item }) return;
        await _page.OpenPostDetailAsync(item);
    }

    // ── The withholding arms' reveals (decided once in FaunaApp.Core —
    //    FeedPostItem.RenderArm → SocialRenderGate — the card only paints the gates). ──

    /// <summary>Reveal a muted post for the session (the mute itself persists — only
    /// un-muting the word stops future collapse). Mutates <c>Revealed</c> in place on the
    /// bound <see cref="FeedPostItem"/> so x:Bind re-renders this row without a full
    /// Reconcile-triggered replace.</summary>
    internal static void RevealMuted(object sender)
    {
        if (sender is not Button { Tag: FeedPostItem item }) return;
        item.Revealed = true;
    }

    /// <summary>Reveal a content-policy, region or nest <b>collapsed</b> post for the
    /// session — one id-keyed session set in <c>ContentPolicyCache</c> shared by all
    /// three sources (family-safety.md § Content policy; region-blocking.md § The blocked
    /// render; nest-rules.md § Additive, never loosenable), so the reveal survives an
    /// observer tick that rebuilds this row into a new item and lifts the composed
    /// <c>collapse</c> whichever source drove it. A <c>block</c> has no such button: the
    /// gate resolves block ahead of the reveal set.</summary>
    internal static void RevealContent(object sender)
    {
        if (sender is not Button { Tag: FeedPostItem item }) return;
        item.RevealContent();
    }

    // ── Convention 17's "a region Block never renders silent" walk
    //    (RegionPlaneHost.BlockRenderState, published as `region_block_render`, and its
    //    nest twin `nest_block_render`). The two sides are registered from DIFFERENT
    //    elements on purpose: the card root carries the verdict side (its item's
    //    composed verdict, whatever arm painted), the placeholder panel the painted side
    //    (only when it is actually shown for a block). An arm that drops or hides the
    //    placeholder then reads as placeholders < blocked. ──

    private string WitnessKey(FeedPostItem item) => _witnessSurface + ":" + item.PostId;

    internal void PostCard_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: FeedPostItem item })
        {
            RegionPlaneHost.WitnessBlocked(WitnessKey(item), item.IsRegionBlocked);
            NestRulesHost.WitnessBlocked(WitnessKey(item), item.IsNestBlocked);
        }
    }

    internal void PostCard_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: FeedPostItem item })
        {
            RegionPlaneHost.WitnessBlocked(WitnessKey(item), false);
            NestRulesHost.WitnessBlocked(WitnessKey(item), false);
        }
    }

    internal void NestPlaceholder_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: FeedPostItem item } panel)
            NestRulesHost.WitnessPainted(
                WitnessKey(item),
                panel.Visibility == Visibility.Visible && item.Nest is { IsBlock: true });
    }

    internal void NestPlaceholder_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: FeedPostItem item })
            NestRulesHost.WitnessPainted(WitnessKey(item), false);
    }

    internal void RegionPlaceholder_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: FeedPostItem item } panel)
            RegionPlaneHost.WitnessPainted(
                WitnessKey(item),
                panel.Visibility == Visibility.Visible && item.Region is { IsBlock: true });
    }

    internal void RegionPlaceholder_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: FeedPostItem item })
            RegionPlaneHost.WitnessPainted(WitnessKey(item), false);
    }

    // UIA Invoke on a nested Button is direct, not pointer-routed (post-card's own
    // comment on load-remote-content-button/feed-post-actions-button), so this never
    // double-fires the post-card's own Click. Reuses the already-loaded bitmap — no
    // second fetch, matching how the card itself painted the image. The Image sits in
    // a Grid beside the bridged-picture placeholder; with no bitmap loaded (that
    // placeholder standing in) there is nothing to enlarge and the click does nothing.
    internal void PostImage_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Content: Panel content }
            && content.Children.OfType<Image>().FirstOrDefault()
                is { Source: BitmapImage bmp })
        {
            _page.ShowImageLightbox(bmp);
        }
    }
}
