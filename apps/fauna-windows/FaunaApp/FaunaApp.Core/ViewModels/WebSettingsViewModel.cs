using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using uniffi.fauna_ffi;
using uniffi.fauna_client_web;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// One <c>web-published-posts-list</c> row (<c>web-content-hosting.md</c>
/// § Published-post management). Wraps the raw <see cref="FfiPublishedPost"/> —
/// <see cref="PostId"/> stays the raw bytes <c>publish.unset</c> takes, never a
/// hex round-trip.
/// </summary>
public sealed class PublishedPostRow
{
    public byte[] PostId { get; }
    public string Slug { get; }
    public string? GatedTier { get; }

    internal PublishedPostRow(FfiPublishedPost p)
    {
        PostId = p.postId;
        Slug = p.slug;
        GatedTier = p.gatedTier;
    }
}

/// <summary>
/// The user <c>web-settings</c> page (web-content-hosting.md § Published-post
/// management; ui.yaml <c>web-settings</c>): the per-user subdomain opt-in
/// PLUS the Published-posts management section (windows
/// leg). A dumb projection over the shared <c>fauna_client_web::WebClient</c>, consumed
/// through its UniFFI <see cref="IFfiWebClient"/> seam (fake it for a deterministic unit
/// test). All decision logic — the reserved-label rule, the origin precedence (active
/// custom domain &gt; enabled subdomain), and the <c>&lt;handle&gt;.&lt;domain&gt;</c>
/// URL — lives in shared Rust (<c>fauna_core::web</c> / <c>fauna_client_web</c>, one
/// source of truth with the nest's routing) and is read via the pure
/// <see cref="FaunaFfiMethods.WebSubdomainView"/> / <see cref="FaunaFfiMethods.WebSiteLinkView"/>
/// projections; this VM never re-derives it in C# (priority #2). Lifts the linux
/// reference (apps/fauna-linux/src/settings/web.rs) + mirrors the web SPA
/// (WebSettingsSection.svelte) and tui (settings/web.rs).
///
/// The toggle is <b>non-optimistic</b>: <see cref="ToggleAsync"/> renders off the
/// nest-confirmed <c>set_subdomain_enabled</c> echo, never the requested value — so the
/// bound <c>web-settings-subdomain-toggle</c> state only flips once the nest confirms.
/// </summary>
public partial class WebSettingsViewModel : ObservableObject
{
    private readonly IFfiWebClient _web;
    private readonly string? _handle;
    /// <summary>The nest's actual serving domain (<c>FfiWebClient.ServingDomain</c>'s
    /// answer), resolved once per load — the correct input to every
    /// <c>subdomain_view</c>/<c>site_link_view</c> projection below, never the cached
    /// sign-in domain directly (web-content-hosting.md § Published-post management:
    /// the cached-domain near-miss is right on a claimed box, wrong — the
    /// <c>"localhost"</c> placeholder — on a domainless one).</summary>
    private string _resolvedDomain = string.Empty;
    private WebDomainRow[] _domains = Array.Empty<WebDomainRow>();
    private SiteLinkView? _siteLink;

    /// <summary><c>web-settings-subdomain-toggle</c> — the per-user opt-in, reflected
    /// from the nest's confirmed state (default OFF) after each load / toggle.</summary>
    [ObservableProperty] private bool _subdomainEnabled;

    /// <summary><c>web-settings-subdomain-url</c> — the live <c>https://&lt;handle&gt;.&lt;domain&gt;/</c>
    /// URL the site serves at, or the disabled-reason explainer (no handle / reserved label).</summary>
    [ObservableProperty] private string _subdomainUrlText = string.Empty;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    // ── Published-posts management section (web-content-hosting.md
    //    § Published-post management). `Domains` + the origin resolution are the
    //    SAME shared projection the feed ⋯-menu reads (`Services.WebOrigin`), so
    //    the two surfaces can never disagree. `Hydrated` is the "nobody has read
    //    yet" gate tui/linux/web use, so a pre-read frame never claims "no
    //    published posts" about a list nobody asked for. ──

    public ObservableCollection<PublishedPostRow> PublishedPosts { get; } = new();
    [ObservableProperty] private bool _hydrated;

    /// <summary>Whether the actor has a serving origin to build copy links on — the
    /// gate every copy affordance's <c>IsEnabled</c> reads (publishing with no origin
    /// is legal but unreachable, and the UI must say so rather than hand out a dead
    /// link).</summary>
    public bool HasOrigin => _siteLink?.@origin is not null;

    /// <summary>The reason beside a dead copy affordance when <see cref="HasOrigin"/>
    /// is false — the settings page's own richer reasons (unlike the feed ⋯-menu,
    /// which cannot point "above" and uses one generic line instead). The mapping
    /// itself lives in shared Rust (<c>fauna_client_web::disabled_reason_text</c>,
    /// via <see cref="FaunaFfiMethods.WebDisabledReasonText"/>) — apple/android/web
    /// already read it through the same door; this VM never re-derives it in C#
    /// (priority #2).</summary>
    public string LinkDisabledReasonText =>
        S.Resolve(FaunaFfiMethods.WebDisabledReasonText(_siteLink?.@disabledReason));

    internal WebSettingsViewModel(IFfiWebClient web, string? handle)
    {
        _web = web;
        _handle = handle;
    }

    /// <summary>Hydrate the subdomain toggle + the Published-posts list, then project the
    /// state. The transport already tolerates the post-login connect race for each single
    /// RPC (transport.md § Request lifecycle step 3) — no app-level retry needed here.</summary>
    public async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            var r = await Services.WebOrigin.ResolveAsync(_web, _handle);
            _resolvedDomain = r.Domain;
            _domains = r.Domains;
            Project(r.SubdomainEnabled, r.SiteLink);

            var posts = await _web.PublishList();
            PublishedPosts.Clear();
            foreach (var p in posts) PublishedPosts.Add(new PublishedPostRow(p));
            Hydrated = true;
        }
        catch (Exception ex)
        {
            Error = S.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Flip the opt-in (<c>web-settings-subdomain-toggle</c> → <c>set_subdomain_enabled</c>).
    /// Non-optimistic: render off the nest-confirmed echo, never the requested value — on
    /// failure the toggle reverts (re-project the last known state) and surfaces the error.
    /// Re-derives the Published-posts section's origin too (the toggle is exactly what
    /// gives those rows' copy affordances an origin at all).</summary>
    public async Task ToggleAsync()
    {
        try
        {
            var echo = await _web.SetSubdomainEnabled(!SubdomainEnabled);
            var siteLink = FaunaFfiMethods.WebSiteLinkView(_domains, echo, _handle, _resolvedDomain);
            Project(echo, siteLink);
        }
        catch (Exception ex)
        {
            Project(SubdomainEnabled, _siteLink!); // revert to the last nest-confirmed state
            Error = S.Error(ex);
        }
    }

    /// <summary>Project the nest-confirmed flag onto the bound state via the shared
    /// <c>fauna_client_web::subdomain_view</c> projection: the toggle position + the live
    /// <c>&lt;handle&gt;.&lt;domain&gt;</c> URL, or the disabled-reason explainer. Also
    /// stores the resolved <c>site_link_view</c> the Published-posts section's copy
    /// affordances key off (<see cref="HasOrigin"/> / <see cref="LinkDisabledReasonText"/>).</summary>
    private void Project(bool enabled, SiteLinkView siteLink)
    {
        SubdomainView view = FaunaFfiMethods.WebSubdomainView(enabled, _handle, _resolvedDomain);
        SubdomainEnabled = view.@enabled;
        SubdomainUrlText = view.@url ?? view.@disabledReason switch
        {
            SubdomainDisabledReason.NoHandle => S.Get("web_settings/subdomain_no_handle"),
            SubdomainDisabledReason.ReservedLabel => S.Get("web_settings/subdomain_reserved"),
            // The nest serves no web content at any host — say so rather than
            // leave the row blank (web-content-hosting.md § Published-post
            // management: legal but unreachable, and the UI must say so).
            SubdomainDisabledReason.NoServingDomain => S.Get("web_settings/subdomain_no_serving_domain"),
            _ => string.Empty,
        };
        _siteLink = siteLink;
        OnPropertyChanged(nameof(HasOrigin));
        OnPropertyChanged(nameof(LinkDisabledReasonText));
    }

    /// <summary>The public page URL for <paramref name="row"/> — pure, no round trip
    /// (the origin and the slug are both already resolved). <c>null</c> when there is
    /// no serving origin.</summary>
    public string? CopyWebLink(PublishedPostRow row)
    {
        if (_siteLink?.@origin is not { } origin) return null;
        return FaunaFfiMethods.WebPostPageUrl(origin, row.Slug);
    }

    /// <summary>Mint + build a short-lived full-access link for <paramref name="row"/>
    /// (gated rows only). A fresh mint per call: the token is short-lived by ratified
    /// design and re-minting is free, so re-copying always yields a link that works
    /// from now rather than a cached one that already expired. <c>null</c> when there
    /// is no serving origin.</summary>
    public async Task<string?> CopyPaywallLinkAsync(PublishedPostRow row)
    {
        if (_siteLink?.@origin is not { } origin) return null;
        Error = null;
        try
        {
            var minted = await _web.PaywallMintToken(new PaywallTarget.PostSlug(row.Slug));
            return FaunaFfiMethods.WebTokenedUrl(origin, minted.@path, minted.@token);
        }
        catch (Exception ex)
        {
            Error = S.Format("web_publish/error_paywall_link", ex.Message);
            return null;
        }
    }

    /// <summary>Take <paramref name="row"/>'s page down, then re-read <c>publish.list</c>
    /// so the section reflects the nest's state — a takedown that half-applied then
    /// shows up as a row that stayed rather than one that vanished from a screen the
    /// nest disagrees with (mirrors linux's <c>unpublish</c>).</summary>
    public async Task UnpublishAsync(PublishedPostRow row)
    {
        Error = null;
        try
        {
            await _web.PublishUnset(row.PostId);
            var posts = await _web.PublishList();
            PublishedPosts.Clear();
            foreach (var p in posts) PublishedPosts.Add(new PublishedPostRow(p));
        }
        catch (Exception ex)
        {
            Error = S.Format("web_publish/error_unpublish", ex.Message);
        }
    }
}
