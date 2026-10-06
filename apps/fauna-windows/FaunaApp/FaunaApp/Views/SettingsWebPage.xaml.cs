using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Web sub-page (web-content-hosting.md § Implementation status → Client
/// authoring UI, Slice 4; § Published-post management, windows leg). Rides the shared,
/// auto-reconnecting WS-RPC connection (the <see cref="INestRpcClient"/> seam's
/// <c>BuildWebClientAsync</c>) for the shared <c>FfiWebClient</c>, and renders the
/// testable <see cref="WebSettingsViewModel"/>: the non-optimistic subdomain toggle +
/// the live URL, PLUS the Published-posts management section
/// (<c>web-published-posts-list</c> of <c>web-published-post-item</c> rows). The
/// toggle's on/off is mirrored to HelpText so the e2e reads it via
/// <c>get_attr(id, "state")</c>; the Published-posts rows follow the same convention
/// for their own <c>disabled</c> / <c>gated-tier</c> / <c>copied</c> attrs.
/// </summary>
public sealed partial class SettingsWebPage : Page
{
    private ServiceClients? _clients;
    private WebSettingsViewModel? _vm;
    private bool _suppressToggle;

    public SettingsWebPage()
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
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e) => await LoadAsync();

    private async Task LoadAsync()
    {
        if (_clients is null) return;
        try
        {
            await EnsureViewModelAsync();
            await _vm!.LoadAsync();
            RenderState();
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
    }

    private async Task EnsureViewModelAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        // Build the shared FfiWebClient over the session's shared, auto-reconnecting
        // WS-RPC connection (the INestRpcClient seam) rather than a per-page one-shot
        // FfiNestClient.Connect() that would surface a transient os-error-10061 as a
        // page error while other pages recover (transport.md — one per-actor WS).
        var web = await _clients.Rpc.BuildWebClientAsync();
        // The handle keys the shared <handle>.<serving domain> URL projection (the
        // windows analog of linux load_account_cache); a handle-less actor renders
        // the disabled reason. Same source the mail panel uses for the MUA username.
        var handle = _clients.Account.Handle;
        _vm = new WebSettingsViewModel(web, handle);
    }

    private void RenderState()
    {
        if (_vm is null) return;
        // Guard suppresses the Toggled re-fire from the programmatic IsOn set.
        _suppressToggle = true;
        SubdomainToggle.IsOn = _vm.SubdomainEnabled;
        _suppressToggle = false;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            SubdomainToggle, _vm.SubdomainEnabled ? "on" : "off");
        SubdomainUrl.Text = _vm.SubdomainUrlText;
        RenderPublishedPosts();
        ShowError(_vm.Error);
    }

    /// <summary>Rebuild the Published-posts section (web-content-hosting.md
    /// § Published-post management) from <see cref="WebSettingsViewModel.PublishedPosts"/>
    /// — imperative, mirroring this page's existing render style (not x:Bind), and
    /// simplest for a list whose row count and per-row enabled-state both change on
    /// every hydrate/toggle. Painted only once the page has actually read the list
    /// (<c>Hydrated</c>) — a pre-read frame must not claim "no published posts"
    /// about a list nobody asked for.</summary>
    private void RenderPublishedPosts()
    {
        if (_vm is null) return;
        if (!_vm.Hydrated)
        {
            PublishedPostsEmpty.Visibility = Visibility.Collapsed;
            PublishedPostsList.Visibility = Visibility.Collapsed;
            return;
        }

        PublishedPostsList.Children.Clear();
        var isEmpty = _vm.PublishedPosts.Count == 0;
        PublishedPostsEmpty.Visibility = isEmpty ? Visibility.Visible : Visibility.Collapsed;
        PublishedPostsList.Visibility = isEmpty ? Visibility.Collapsed : Visibility.Visible;

        var hasOrigin = _vm.HasOrigin;
        foreach (var row in _vm.PublishedPosts)
        {
            // controls:WrapStack, not a horizontal StackPanel: a gated row (slug +
            // tier badge + copy-link + copy-paywall-link + unpublish, five children)
            // does not fit the page's MaxWidth="600" content column, and a StackPanel
            // overflows (clipped, UIA IsOffscreen=true) rather than wrapping — the
            // same shape WrapStack.cs documents for the onboarding DNS provider row.
            // The outer ScrollViewer is vertical-only, so StartBringIntoView cannot
            // rescue a horizontal overflow (flaui-bridge's scroll-into-view sweep
            // only walks VerticallyScrollable ancestors) — wrapping removes the
            // overflow instead of chasing it.
            var item = new FaunaApp.Controls.WrapStack { HorizontalSpacing = 8, VerticalSpacing = 4 };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(item, Ids.WebPublishedPostItem);
            // A DataTemplate/layout panel carrying only an AutomationId is UIA-pruned
            // (reference_winui_flaui_datatemplate_name) — Name keeps it discoverable
            // AND is what get_attr(ROW, "gated-tier", index=i) is really reading:
            // "gated-tier" isn't "state"/"disabled", so it maps generically to
            // HelpText, the one extra string this element carries.
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, row.Slug);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(item, row.GatedTier ?? "");

            var slugText = new TextBlock { Text = row.Slug, VerticalAlignment = VerticalAlignment.Center };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(slugText, Ids.WebPublishedPostSlug);
            item.Children.Add(slugText);

            if (row.GatedTier is { Length: > 0 } tier)
            {
                item.Children.Add(new TextBlock
                {
                    Text = S.Format("web_settings/published_post_gated_badge", tier),
                    Opacity = 0.7,
                    VerticalAlignment = VerticalAlignment.Center,
                });
            }

            var copyWeb = new Button
            {
                Content = S.Get("web_publish/copy_web_link"),
                IsEnabled = hasOrigin,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(copyWeb, Ids.WebPublishedPostCopyLinkButton);
            copyWeb.Click += (_, _) =>
            {
                var url = _vm.CopyWebLink(row);
                if (url is null) return;
                FaunaApp.Helpers.ClipboardHelper.CopyText(url);
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(copyWeb, url);
            };
            item.Children.Add(copyWeb);

            // Gated rows only: an ungated post has no paywalled body, so the mint
            // would hand out a token for nothing.
            if (row.GatedTier is { Length: > 0 })
            {
                var copyPaywall = new Button
                {
                    Content = S.Get("web_publish/copy_paywall_link"),
                    IsEnabled = hasOrigin,
                };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(copyPaywall, Ids.WebPublishedPostCopyPaywallLinkButton);
                copyPaywall.Click += async (_, _) =>
                {
                    var url = await _vm.CopyPaywallLinkAsync(row);
                    if (url is null) { ShowError(_vm.Error); return; }
                    FaunaApp.Helpers.ClipboardHelper.CopyText(url);
                    Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(copyPaywall, url);
                };
                item.Children.Add(copyPaywall);
            }

            // Take a published post down, then re-read the list so the section
            // reflects the nest's state — a takedown that half-applied then shows
            // up as a row that stayed rather than one that vanished from a screen
            // the nest disagrees with. One tap, no confirm step (idempotent and
            // reversible, unlike delete).
            var unpublish = new Button { Content = S.Get("web_publish/unpublish") };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(unpublish, Ids.WebPublishedPostUnpublishButton);
            unpublish.Click += async (_, _) =>
            {
                await _vm.UnpublishAsync(row);
                RenderState();
            };
            item.Children.Add(unpublish);

            PublishedPostsList.Children.Add(item);
        }
    }

    private async void SubdomainToggle_Toggled(object sender, RoutedEventArgs e)
    {
        // Non-optimistic: the VM flips the caller-scoped set_subdomain_enabled WS-RPC and
        // re-projects off the nest echo, so RenderState reflects the confirmed value (and
        // reverts the toggle on failure). Guarded against the programmatic IsOn set.
        if (_suppressToggle || _vm is null) return;
        await _vm.ToggleAsync();
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
