using System.Threading.Tasks;
using uniffi.fauna_client_web;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Resolves the shared "where does this actor's published content live"
/// projection (<c>fauna_client_web::site_link_view</c>) — the one answer both
/// the <c>web-settings</c> Published-posts section and the feed ⋯-menu's copy
/// verbs render off, so the two surfaces can never disagree about a creator's
/// address (mirrors tui's <c>crate::settings::web::site_link</c> / web's
/// <c>$siteLink</c> store; <c>web-content-hosting.md</c> § Published-post
/// management).
///
/// The domain is the nest's own <c>fauna.nest.info</c> <c>web_serving_domain</c>
/// (<see cref="IFfiWebClient.ServingDomain"/>). Passing the cached sign-in
/// domain as the resolver input (what this page used to do) is the exact
/// near-miss <c>web-content-hosting.md</c> § Published-post management
/// names: right on a claimed box, wrong (the <c>"localhost"</c> placeholder) on
/// a domainless one.
/// </summary>
// internal (not public): every parameter/return type here is a UniFFI-generated
// `internal` type (IFfiWebClient, WebDomainRow, SiteLinkView), so a public
// signature would be less accessible than its own types (CS0051) — the same
// reason WebSettingsViewModel's own constructor is `internal` on an otherwise
// public class. FaunaApp/FaunaApp.Tests see it via [InternalsVisibleTo].
internal static class WebOrigin
{
    internal static async Task<Resolved> ResolveAsync(
        IFfiWebClient web, string? handle)
    {
        var domain = await web.ServingDomain();
        var subdomainEnabled = await web.GetSubdomainEnabled();
        var domains = await web.DomainGet();
        var siteLink = FaunaFfiMethods.WebSiteLinkView(domains, subdomainEnabled, handle, domain);
        return new Resolved(domain, subdomainEnabled, domains, siteLink);
    }

    internal sealed record Resolved(
        string Domain, bool SubdomainEnabled, WebDomainRow[] Domains, SiteLinkView SiteLink);
}
