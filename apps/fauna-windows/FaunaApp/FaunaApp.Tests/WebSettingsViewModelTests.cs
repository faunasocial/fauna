using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using uniffi.fauna_client_web;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IFfiWebClient"/> for the web-authoring view-model tests — the
/// UniFFI web-client seam, faked (no native nest). Stateful: set/get round-trip the
/// per-user subdomain flag + the nest-wide apex actor, recording every set request.
/// Set <see cref="ForceSetSubdomainResult"/> to model a nest that refuses/pins the flip,
/// proving the toggle renders the echo (non-optimistic) not the request. Shared by
/// <see cref="WebSettingsViewModelTests"/> + <see cref="AdminWebViewModelTests"/>.
/// </summary>
internal sealed class FakeWebClient : FfiWebClientFakeBase
{
    public bool SubdomainState;
    public bool? ForceSetSubdomainResult;
    public byte[]? ApexActor;
    public readonly List<bool> SubdomainSetRequests = new();
    public readonly List<byte[]?> ApexSetRequests = new();

    /// <summary>The nest's <c>ServingDomain</c> answer.</summary>
    public string NestServingDomain = "example.test";
    public WebDomainRow[] CustomDomains = Array.Empty<WebDomainRow>();
    public FfiPublishedPost[] Published = Array.Empty<FfiPublishedPost>();
    public readonly List<byte[]> PublishUnsetRequests = new();
    public readonly List<string> PaywallMintRequests = new();

    public override Task<bool> GetSubdomainEnabled() => Task.FromResult(SubdomainState);

    public override Task<bool> SetSubdomainEnabled(bool @enabled)
    {
        SubdomainSetRequests.Add(@enabled);
        SubdomainState = ForceSetSubdomainResult ?? @enabled;
        return Task.FromResult(SubdomainState);
    }

    public override Task<byte[]?> GetApexActor() => Task.FromResult(ApexActor);

    public override Task<byte[]?> SetApexActor(byte[]? @actorId)
    {
        ApexSetRequests.Add(@actorId);
        ApexActor = @actorId;
        return Task.FromResult(ApexActor);
    }

    public override Task<string> ServingDomain() => Task.FromResult(NestServingDomain);

    public override Task<WebDomainRow[]> DomainGet() => Task.FromResult(CustomDomains);

    public override Task<FfiPublishedPost[]> PublishList() => Task.FromResult(Published);

    public override Task<bool> PublishUnset(byte[] @postId)
    {
        PublishUnsetRequests.Add(@postId);
        Published = Published.Where(p => !p.postId.AsSpan().SequenceEqual(@postId)).ToArray();
        return Task.FromResult(true);
    }

    public override Task<MintedPaywallLink> PaywallMintToken(PaywallTarget @target)
    {
        var slug = target is PaywallTarget.PostSlug s ? s.@slug : "";
        PaywallMintRequests.Add(slug);
        return Task.FromResult(new MintedPaywallLink(token: "test-token", expires: 9999999999, path: $"post/{slug}.html"));
    }
}

/// <summary>web-content-hosting.md § Published-post management:
/// the windows <c>web-settings</c> subdomain toggle. Mirrors the tier_3 e2e
/// <c>test_web_authoring.py::test_user_subdomain_toggle_round_trip</c> at the VM level — and
/// this VM test is the only GATED witness: win's windows-cs-test-compile gate runs
/// FaunaApp.Tests, while no gate on any machine runs Python e2e (merge-gate-check.md's
/// accepted gap), so the FlaUI e2e fires only when a session runs the suite by hand.
/// (The old text here called the FlaUI e2e "the CI gate" — there is no automatic CI.)</summary>
public class WebSettingsViewModelTests
{
    [Fact]
    public async Task Subdomain_toggle_round_trips_from_default_off()
    {
        var web = new FakeWebClient { SubdomainState = false };
        var vm = new WebSettingsViewModel(web, "alice");

        await vm.LoadAsync();
        Assert.False(vm.SubdomainEnabled); // default OFF (privacy)

        await vm.ToggleAsync();
        Assert.True(vm.SubdomainEnabled); // ON after the nest round-trip
        Assert.Equal(new[] { true }, web.SubdomainSetRequests);

        await vm.ToggleAsync();
        Assert.False(vm.SubdomainEnabled); // back OFF (leaves the actor as found)
    }

    [Fact]
    public async Task Subdomain_toggle_is_non_optimistic()
    {
        // The nest refuses the flip (pins OFF). The toggle must render that echo, not the
        // requested ON — proving it is non-optimistic (the e2e reads `state` off the echo).
        var web = new FakeWebClient { SubdomainState = false, ForceSetSubdomainResult = false };
        var vm = new WebSettingsViewModel(web, "alice");
        await vm.LoadAsync();

        await vm.ToggleAsync();
        Assert.True(web.SubdomainSetRequests.Single()); // it asked the nest for ON…
        Assert.False(vm.SubdomainEnabled); // …but renders the nest's OFF echo
    }

    [Fact]
    public async Task Subdomain_url_uses_the_shared_projection_for_a_valid_handle()
    {
        var web = new FakeWebClient { SubdomainState = true };
        var vm = new WebSettingsViewModel(web, "alice");

        await vm.LoadAsync();
        // The live URL comes from the shared fauna_core::web::subdomain_url projection
        // (real native FfiMethods call) — never re-derived in C#.
        Assert.Contains("alice.example.test", vm.SubdomainUrlText);
    }

    // ── Published-posts management section (web-content-hosting.md
    //    § Published-post management, windows leg) ──────────────────────────

    /// Widening <see cref="FfiPublishedPost"/> touches exactly this one factory
    /// , instead of every
    /// call site in this file hand-listing all three fields positionally.
    private static FfiPublishedPost Post(byte[] postId, string slug, string? gatedTier = null) =>
        new FfiPublishedPost(postId, slug, gatedTier);

    [Fact]
    public async Task Published_posts_hydrate_from_publish_list()
    {
        var web = new FakeWebClient
        {
            SubdomainState = true,
            Published = new[] { Post(new byte[] { 1, 2, 3 }, "my-page") },
        };
        var vm = new WebSettingsViewModel(web, "alice");

        await vm.LoadAsync();

        Assert.True(vm.Hydrated);
        var row = Assert.Single(vm.PublishedPosts);
        Assert.Equal("my-page", row.Slug);
        Assert.Null(row.GatedTier);
    }

    [Fact]
    public async Task Copy_web_link_is_dead_with_no_serving_origin()
    {
        // Publishing with no serving origin is legal but unreachable — subdomain
        // OFF and no custom domain means HasOrigin must be false and the copy
        // affordance must hand back null rather than a dead link.
        var web = new FakeWebClient
        {
            SubdomainState = false,
            Published = new[] { Post(new byte[] { 1 }, "p") },
        };
        var vm = new WebSettingsViewModel(web, "alice");
        await vm.LoadAsync();

        Assert.False(vm.HasOrigin);
        Assert.Null(vm.CopyWebLink(vm.PublishedPosts.Single()));
    }

    [Fact]
    public async Task Link_disabled_reason_text_comes_from_the_shared_ffi_projection()
    {
        // No localizer is initialized in these tests, so `S.Resolve` falls back to
        // the raw DOTTED `LocalizedText` key -- proving the value round-tripped
        // through the shared `fauna_client_web::disabled_reason_text` door (a real
        // native FFI call), not a hand-rolled C# switch (which fell back to the
        // SLASHED key via `S.Get` instead) — the same real-FFI-call pattern
        // `Subdomain_url_uses_the_shared_projection_for_a_valid_handle` pins.
        var web = new FakeWebClient { SubdomainState = false };
        var vm = new WebSettingsViewModel(web, "alice");
        await vm.LoadAsync();

        Assert.False(vm.HasOrigin);
        Assert.Equal("web_settings.link_disabled_subdomain_off", vm.LinkDisabledReasonText);
    }

    [Fact]
    public async Task Link_disabled_reason_text_distinguishes_no_handle_from_subdomain_off()
    {
        var web = new FakeWebClient { SubdomainState = true };
        var vm = new WebSettingsViewModel(web, null);
        await vm.LoadAsync();

        Assert.False(vm.HasOrigin);
        Assert.Equal("web_settings.link_disabled_no_handle", vm.LinkDisabledReasonText);
    }

    [Fact]
    public async Task Copy_web_link_addresses_this_actors_own_origin_and_effective_slug()
    {
        var web = new FakeWebClient
        {
            SubdomainState = true,
            Published = new[] { Post(new byte[] { 1 }, "my-page") },
        };
        var vm = new WebSettingsViewModel(web, "alice");
        await vm.LoadAsync();

        Assert.True(vm.HasOrigin);
        var url = vm.CopyWebLink(vm.PublishedPosts.Single());
        Assert.StartsWith("https://alice.", url);
        Assert.EndsWith("/post/my-page.html", url);
    }

    [Fact]
    public async Task Copy_paywall_link_mints_a_fresh_token_scoped_to_the_slug()
    {
        var web = new FakeWebClient
        {
            SubdomainState = true,
            Published = new[] { Post(new byte[] { 1 }, "sold-page", gatedTier: "gold") },
        };
        var vm = new WebSettingsViewModel(web, "alice");
        await vm.LoadAsync();

        var url = await vm.CopyPaywallLinkAsync(vm.PublishedPosts.Single());

        Assert.NotNull(url);
        Assert.Contains("post/sold-page.html?token=test-token", url);
        Assert.Equal(new[] { "sold-page" }, web.PaywallMintRequests);
    }

    [Fact]
    public async Task Unpublish_removes_the_row_and_commits_to_the_nest()
    {
        var postId = new byte[] { 9, 9 };
        var web = new FakeWebClient
        {
            SubdomainState = true,
            Published = new[] { Post(postId, "gone-soon") },
        };
        var vm = new WebSettingsViewModel(web, "alice");
        await vm.LoadAsync();

        await vm.UnpublishAsync(vm.PublishedPosts.Single());

        Assert.Empty(vm.PublishedPosts);
        Assert.Equal(postId, Assert.Single(web.PublishUnsetRequests));
    }
}
