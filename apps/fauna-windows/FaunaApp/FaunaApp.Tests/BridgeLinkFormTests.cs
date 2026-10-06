using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Models;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The windows Bridges link form is metadata-driven: it renders the active
/// <see cref="BridgeLinkMode"/>'s fields generically, with no per-bridge hard-coded
/// switch (bridges.md § Element IDs + § Implementation status today; priority
/// #1/#2/#4 — the same shared shape linux/web/android/apple already render). These
/// tests pin the platform-side model logic (<see cref="BridgeLinkForm"/>): which
/// modes render on windows, and the <c>bridge-link-field-{key}</c> test ids derived
/// from the raw provider field keys. The XAML rendering itself is build- + e2e-verified.
/// </summary>
public class BridgeLinkFormTests
{
    private static BridgeInfo Bridge(string id, params BridgeLinkMode[] modes) =>
        new(id, id, Available: true, Linked: false, Identity: null, Mode: null, LinkModes: modes.ToList(),
            Settings: new List<BridgeSetting>());

    private static BridgeLinkField Field(string key, string type = "text", string? placeholder = null) =>
        new(key, $"{key} label", type, placeholder);

    // Bluesky's live feed-side mode: a single `oauth` mode with one `handle` text field.
    private static readonly BridgeLinkMode BlueskyOauth =
        new("oauth", "OAuth", "oauth_redirect", Platform: null, Fields: new[] { Field("handle", placeholder: "@you.bsky.social") }.ToList());

    // ActivityPub's live feed-side mode: a single `enable` mode declaring NO fields.
    private static readonly BridgeLinkMode ActivityPubEnable =
        new("enable", "Enable", null, Platform: null, Fields: new List<BridgeLinkField>());

    // Nostr's web-only NIP-07 mode: platform-scoped to "web", must NOT render on windows.
    private static readonly BridgeLinkMode Nip07Web =
        new("nip07", "Browser extension", "nip07", Platform: "web", Fields: new List<BridgeLinkField>());

    [Fact]
    public void ModesForPlatform_KeepsUniversalAndWindowsModes()
    {
        var windowsOnly = new BridgeLinkMode("native", "Native", null, Platform: "windows", Fields: new List<BridgeLinkField>());
        var bridge = Bridge("x", BlueskyOauth, windowsOnly);

        var modes = BridgeLinkForm.ModesForPlatform(bridge);

        Assert.Equal(2, modes.Count);
        Assert.Contains(modes, m => m.Mode == "oauth");
        Assert.Contains(modes, m => m.Mode == "native");
    }

    [Fact]
    public void ModesForPlatform_DropsWebOnlyModes()
    {
        var bridge = Bridge("nostr", BlueskyOauth, Nip07Web);

        var modes = BridgeLinkForm.ModesForPlatform(bridge);

        Assert.Single(modes);
        Assert.Equal("oauth", modes[0].Mode);
    }

    [Fact]
    public void FieldTestId_UsesRawProviderKey()
    {
        Assert.Equal("bridge-link-field-handle", BridgeLinkForm.FieldTestId(Field("handle")));
        Assert.Equal("bridge-link-field-relay_list", BridgeLinkForm.FieldTestId(Field("relay_list")));
    }

    [Fact]
    public void BlueskyShape_RendersSingleHandleTextField()
    {
        var bridge = Bridge("bluesky", BlueskyOauth);

        var mode = Assert.Single(BridgeLinkForm.ModesForPlatform(bridge));
        var field = Assert.Single(mode.Fields);

        Assert.Equal("oauth", mode.Mode);
        Assert.Equal("handle", field.Key);
        Assert.False(field.IsSecret);
        Assert.Equal("bridge-link-field-handle", BridgeLinkForm.FieldTestId(field));
    }

    [Fact]
    public void ActivityPubShape_RendersNoFields()
    {
        var bridge = Bridge("activitypub", ActivityPubEnable);

        var mode = Assert.Single(BridgeLinkForm.ModesForPlatform(bridge));

        Assert.Equal("enable", mode.Mode);
        Assert.Empty(mode.Fields);
    }

    [Fact]
    public void SecretField_IsSecret()
    {
        Assert.True(Field("app_password", type: "secret").IsSecret);
        Assert.False(Field("handle", type: "text").IsSecret);
    }
}
