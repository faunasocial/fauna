using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The standalone Nostr settings page VM (docs/goal/ui/nostr.md § Page structure /
/// § Layout &amp; flow / § User actions). windows was the last client with no Nostr
/// surface at all; these cases pin the three invariants this page's history on the
/// OTHER apps made mandatory, so windows cannot re-introduce a bug already paid
/// for elsewhere:
/// <list type="number">
/// <item><b><c>available</c> gates nothing; only <c>registered</c> does</b> — the
/// web/apple bootstrap bug of 2026-07-14, where the nsec-deposit gate hid the very
/// form that bootstraps the first deposit.</item>
/// <item><b>The link gate really swaps the surface</b> — the linux bug of
/// 2026-07-19, where every row rendered unconditionally so the e2e's
/// <c>is_linked()</c> signal was meaningless.</item>
/// <item><b>Non-optimistic writes</b> — state comes from the nest re-read, not the
/// tap. <see cref="MockNestRpcClient.BridgesSetSettingsAsync"/> genuinely persists
/// into its roster, so an echo-the-request VM would fail these.</item>
/// <item><b>Connected apps is gated on CUSTODY, not merely on being linked</b> — and
/// the gate is load-bearing enough that the roster is not even fetched for a
/// <c>remote</c> account, whose key is not on this box for the box to sign with
/// (nostr.md § The nest as the user's NIP-46 signer). Its roster labels route through
/// the shared <c>bunker_app_label</c>/<c>bunker_last_used_label</c> Rust faces rather
/// than a per-app ternary.</item>
/// </list>
///
/// <para><c>[Collection("StringsGlobal")]</c> because the Connected-apps roster case
/// resolves shared <c>LocalizedText</c> through <see cref="Strings"/>, whose localizer
/// is process-global: a sibling class calling <c>Strings.Initialize</c> between this
/// test's two resolutions of the same key would make it flake. Serializing costs
/// nothing here (these are sub-millisecond pure-projection cases) and buys a
/// deterministic verdict.</para>
/// </summary>
[Collection("StringsGlobal")]
public class NostrViewModelTests
{
    private static BridgeSetting Flag(string key, bool v) => new(key, key, "bool", v, null);

    private static BridgeSetting RelayList(params string[] urls) =>
        new("relay_list", "Relay list", "text", null, JsonSerializer.Serialize(urls));

    /// <summary>A nostr bridge row shaped like <c>bridge_provider.rs</c> emits it:
    /// the 5 bool flags plus the <c>relay_list</c> JSON-array-in-a-text-value.</summary>
    private static BridgeInfo Nostr(
        bool available = false, bool linked = false, string? mode = null,
        IEnumerable<BridgeSetting>? settings = null) =>
        new("nostr", "Nostr", available, linked, linked ? "npub1abc...wxyz" : null, mode,
            new List<BridgeLinkMode>(),
            (settings ?? new[]
            {
                Flag("expose_content", false), Flag("auto_publish", false),
                Flag("publish_replies", false), Flag("publish_reactions", false),
                Flag("inbound_to_feed", false), RelayList(),
            }).ToList());

    private static BridgeInfo Other(string id) =>
        new(id, id, true, false, null, null, new List<BridgeLinkMode>(), new List<BridgeSetting>());

    // ── (1) registered vs available ──

    [Fact]
    public async Task Unavailable_Bridge_IsStillRegistered_SoTheLinkFormRenders()
    {
        // The e2e nest's real shape: the nostr bridge IS registered (built with the
        // `nostr` cargo feature) but `available` is false because nobody has deposited
        // an nsec yet. The link form MUST render — it is what bootstraps that deposit.
        var rpc = new MockNestRpcClient { NextBridges = new[] { Nostr(available: false) } };
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.True(vm.Registered);
        Assert.False(vm.Available);
        Assert.False(vm.Linked);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task MissingBridge_IsNotRegistered()
    {
        // A nest built without the `nostr` cargo feature: the bridge never appears in
        // the roster at all. This — and only this — is the "unavailable" notice's gate.
        var rpc = new MockNestRpcClient { NextBridges = new[] { Other("bluesky") } };
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(vm.Registered);
        Assert.False(vm.Linked);
    }

    // ── (2) the link gate ──

    [Fact]
    public async Task LinkThenUnlink_FlipsTheGate_AndRoundTripsThroughTheNest()
    {
        var rpc = new MockNestRpcClient { NextBridges = new[] { Nostr() } };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();
        Assert.False(vm.Linked);

        // The nest links and the roster now reports linked+generated.
        rpc.NextBridges = new[] { Nostr(linked: true, mode: "generated") };
        await vm.LinkAsync(null);

        Assert.Contains("BridgesLink", rpc.Calls);
        Assert.Equal(("nostr", "generate"), (rpc.LastLink!.Value.BridgeId, rpc.LastLink!.Value.Mode));
        Assert.True(vm.Linked);
        Assert.True(vm.ShowConnectedApps);   // linked + custodial

        rpc.NextBridges = new[] { Nostr() };
        await vm.UnlinkAsync();

        Assert.Contains("BridgesUnlink", rpc.Calls);
        Assert.False(vm.Linked);
        Assert.False(vm.ShowConnectedApps);
    }

    [Fact]
    public async Task ConnectedApps_HiddenForARemoteAccount()
    {
        // A `remote` (external-bunker) account keeps its key off this box, so the box
        // can never be that account's signer (nostr.md § Layout & flow item 6).
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "remote") },
        };
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.True(vm.Linked);
        Assert.False(vm.ShowConnectedApps);
    }

    [Fact]
    public async Task ImportMode_WithoutAnNsec_ErrorsAndSendsNothing()
    {
        var rpc = new MockNestRpcClient { NextBridges = new[] { Nostr() } };
        var vm = new NostrViewModel(rpc) { LinkMode = NostrViewModel.ModeImport };

        await vm.LinkAsync("   ");

        Assert.DoesNotContain("BridgesLink", rpc.Calls);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }

    [Fact]
    public async Task ImportMode_SendsTheTrimmedNsecAsALinkField()
    {
        var rpc = new MockNestRpcClient { NextBridges = new[] { Nostr() } };
        var vm = new NostrViewModel(rpc) { LinkMode = NostrViewModel.ModeImport };

        await vm.LinkAsync("  nsec1example  ");

        Assert.Equal("import", rpc.LastLink!.Value.Mode);
        Assert.Equal("nsec1example", rpc.LastLink!.Value.Fields["nsec"]);
    }

    // ── Succession-aftermath npub confirm (leg 3, nostr.md § Key succession
    // and rotation) ──

    [Fact]
    public async Task NpubConfirmationOwed_ProjectsFromTheNest_AndBannerNamesTheNpub()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
            NextNpubConfirmationOwed = true,
        };
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.True(vm.NpubConfirmationOwed);
        // A self-contained notice, not just a pointer at the row above (the
        // i18n comment on nostr/npub_confirm/banner) — computed the same way
        // the VM does, so this holds regardless of the process-global
        // localizer's current state (raw-key echo or a real resw lookup).
        Assert.Equal(
            Strings.Get("nostr/npub_confirm/banner").Replace("{npub}", "npub1abc...wxyz"),
            vm.NpubConfirmBannerText);
    }

    [Fact]
    public async Task NpubConfirmationOwed_DefaultsToFalse_TheOrdinaryCase()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
        };
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(vm.NpubConfirmationOwed);
    }

    [Fact]
    public async Task ConfirmNpub_RecordsTheConfirmation_ThenReReads_NonOptimistically()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
            NextNpubConfirmationOwed = true,
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();
        Assert.True(vm.NpubConfirmationOwed);

        // The nest now reports the confirmation recorded — the VM must re-read
        // rather than assume, same as every other mutation on this page.
        rpc.NextNpubConfirmationOwed = false;
        await vm.ConfirmNpubAsync();

        Assert.Equal(1, rpc.ConfirmNpubCallCount);
        Assert.False(vm.NpubConfirmationOwed);
    }

    [Fact]
    public async Task DismissNpubToNewKey_ReusesTheExistingUnlinkGesture_NoBespokeFlow()
    {
        // nostr.md:75 is explicit the "no / nothing is linked" remedy is "the
        // existing page machinery" — tui's Action::DismissNpubToNewKey maps
        // directly to Op::Unlink, and windows has no separate dismiss method
        // at all: the button is wired straight to Unlink_Click.
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
            NextNpubConfirmationOwed = true,
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();

        // What the real predicate would answer once unlinked — the mock does
        // not derive this itself, so the test states it explicitly.
        rpc.NextBridges = new[] { Nostr() };
        rpc.NextNpubConfirmationOwed = false;
        await vm.UnlinkAsync();

        Assert.Contains("BridgesUnlink", rpc.Calls);
        Assert.False(vm.Linked);
        Assert.False(vm.NpubConfirmationOwed);
    }

    // ── (3) content toggles, non-optimistically ──

    [Fact]
    public async Task SetToggle_PersistsOnlyTheChangedKey_AndRereads()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();
        Assert.False(vm.Toggle("auto_publish"));

        await vm.SetToggleAsync("auto_publish", true);

        // Exactly one key written — the provider's wire type is all-optional, so a
        // full read-modify-write would risk clobbering a flag changed elsewhere.
        var written = rpc.LastSetSettings!.Value.Settings;
        Assert.Equal("nostr", rpc.LastSetSettings!.Value.BridgeId);
        Assert.Single(written);
        Assert.Equal("auto_publish", written[0].Key);
        Assert.True(written[0].BoolValue);

        // And the new value came back from the re-read, not from the request.
        Assert.True(vm.Toggle("auto_publish"));
        Assert.False(vm.Toggle("expose_content"));
    }

    [Fact]
    public async Task SetToggle_ShowsWhatTheNestPersisted_NotWhatWasTapped()
    {
        // The decisive non-optimistic assertion: the nest is made to report the
        // OPPOSITE of the requested value (a provider that refuses or clamps). A VM
        // that echoed the tap would read `true` here; one that re-reads reads `false`.
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
            // A nest that accepts the write and then keeps reporting the old value.
            PersistSetSettings = false,
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();

        await vm.SetToggleAsync("auto_publish", true);

        Assert.Contains("BridgesSetSettings", rpc.Calls);
        Assert.False(vm.Toggle("auto_publish"));
    }

    [Fact]
    public async Task AllFiveContentToggles_AreProjected()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[]
            {
                Nostr(linked: true, mode: "generated", settings: new[]
                {
                    Flag("expose_content", true), Flag("auto_publish", false),
                    Flag("publish_replies", true), Flag("publish_reactions", false),
                    Flag("inbound_to_feed", true), RelayList(),
                }),
            },
        };
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        // The five-row shape is now the shared catalog's, not a VM-local table
        // (nostr.md § Where logic lives → *The content-toggle catalog*).
        Assert.Equal(5, FaunaFfiMethods.NostrContentToggleOptions().Length);
        Assert.True(vm.Toggle("expose_content"));
        Assert.False(vm.Toggle("auto_publish"));
        Assert.True(vm.Toggle("publish_replies"));
        Assert.False(vm.Toggle("publish_reactions"));
        Assert.True(vm.Toggle("inbound_to_feed"));
    }

    /// <summary>A never-configured account: the nest reports NOTHING for the five
    /// content flags (only <c>relay_list</c>). The catalog's own <c>default_on</c>
    /// mirrors the NEST's default — <c>publish_replies</c>/<c>inbound_to_feed</c> are
    /// ON there — so a client-side `false` fallback would paint the opposite of what
    /// the nest would actually do for an unreported key.</summary>
    [Fact]
    public async Task UnreportedToggle_FallsBackToTheCatalogsNestDefault_NotClientSideFalse()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[]
            {
                Nostr(linked: true, mode: "generated", settings: new[] { RelayList() }),
            },
        };
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(vm.Toggle("expose_content"));
        Assert.False(vm.Toggle("auto_publish"));
        Assert.True(vm.Toggle("publish_replies"));
        Assert.False(vm.Toggle("publish_reactions"));
        Assert.True(vm.Toggle("inbound_to_feed"));
    }

    // ── Relays ──

    [Fact]
    public async Task Relays_ProjectFromTheRelayListJsonString()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[]
            {
                Nostr(linked: true, mode: "generated", settings: new[]
                {
                    Flag("expose_content", false), Flag("auto_publish", false),
                    Flag("publish_replies", false), Flag("publish_reactions", false),
                    Flag("inbound_to_feed", false),
                    RelayList("wss://a.example.com", "wss://b.example.com"),
                }),
            },
        };
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal(
            new[] { "wss://a.example.com", "wss://b.example.com" },
            vm.Relays.Select(r => r.Url));
    }

    [Fact]
    public async Task AddRelay_RejectsANonWebsocketUrl_ClientSide()
    {
        // The shared refusal (fauna_protocol::nostr_relay::relay_url_error) — no
        // per-app prefix hand-roll. No row, no write, an error surfaced.
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();

        await vm.AddRelayAsync("http://not-a-relay.example.com");

        Assert.Empty(vm.Relays);
        Assert.DoesNotContain("BridgesSetSettings", rpc.Calls);
        var expected = Strings.Resolve(FaunaFfiMethods.RelayUrlError("http://not-a-relay.example.com")!);
        Assert.Contains("invalid_url", expected);
        Assert.Equal(expected, vm.ErrorMessage);
    }

    [Theory]
    [InlineData("wss://192.168.1.10:7777")]
    [InlineData("wss://localhost")]
    public async Task AddRelay_RefusesAPrivateAddress_WithTheSharedPrivateAddressMessage(string url)
    {
        // relay_url_error owns the message too (network-exposure.md § Rulings F7): a
        // well-formed relay no dial may reach shows private_address, not invalid_url.
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();

        await vm.AddRelayAsync(url);

        Assert.Empty(vm.Relays);
        Assert.DoesNotContain("BridgesSetSettings", rpc.Calls);
        var expected = Strings.Resolve(FaunaFfiMethods.RelayUrlError(url)!);
        Assert.Contains("private_address", expected);
        Assert.Equal(expected, vm.ErrorMessage);
    }

    [Fact]
    public async Task AddThenRemoveRelay_RoundTripsThroughTheNest()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();

        await vm.AddRelayAsync("  wss://relay.example.com  ");

        Assert.Equal(new[] { "wss://relay.example.com" }, vm.Relays.Select(r => r.Url));
        // Written as a JSON array inside a text value, matching bridge_provider.rs.
        Assert.Equal(
            "[\"wss://relay.example.com\"]",
            rpc.LastSetSettings!.Value.Settings.Single(s => s.Key == "relay_list").TextValue);

        await vm.RemoveRelayAsync(0);

        Assert.Empty(vm.Relays);
        Assert.Equal("[]", rpc.LastSetSettings!.Value.Settings.Single(s => s.Key == "relay_list").TextValue);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("not json")]
    [InlineData("{\"nope\":1}")]
    public void ParseRelayList_IsTotal_OverAMalformedSetting(string? json)
    {
        // A settings blob a future nest grew must never break the page.
        Assert.Empty(NostrViewModel.ParseRelayList(json));
    }

    // ── Follows ──

    [Fact]
    public async Task Follows_AreOnlyFetchedForALinkedAccount()
    {
        var rpc = new MockNestRpcClient { NextBridges = new[] { Nostr() } };
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.DoesNotContain("BridgesListFollows", rpc.Calls);
        Assert.Empty(vm.Follows);
    }

    [Fact]
    public async Task AddThenRemoveFollow_RoundTripsThroughTheNest()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
            NextFollows = new[] { new BridgeFollow("npub1alice", "alice") },
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();

        Assert.Single(vm.Follows);

        await vm.AddFollowAsync("  00112233  ", "  alice  ");
        Assert.Contains("BridgesAddFollow", rpc.Calls);

        rpc.NextFollows = new List<BridgeFollow>();
        await vm.RemoveFollowAsync(0);

        Assert.Contains("BridgesRemoveFollow", rpc.Calls);
        Assert.Empty(vm.Follows);
    }

    // ── Connected apps (Nostr Connect / NIP-46 bunker) ──
    //
    // nostr.md § The nest as the user's NIP-46 signer. This page keeps only the
    // invite start; the connections are signer rows on the Connected apps page
    // (connected-apps.md), whose view-model tests pin their roster and revoke.

    [Fact]
    public async Task ConnectApp_RevealsTheOneTimeConnectString()
    {
        // The mint's whole point is the one-time reveal (§ Connection model): the nest
        // never serves that string again.
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();

        await vm.ConnectAppAsync();

        Assert.Contains("NostrBunkerCreateInvite", rpc.Calls);
        Assert.StartsWith("bunker://", vm.ConnectString);
        Assert.Contains("relay=", vm.ConnectString);
        Assert.Contains("secret=", vm.ConnectString);
        Assert.Null(vm.ErrorMessage);
    }

    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    /// <summary>The link-*request*-mode picker's label map is single-sourced in
    /// <c>fauna_client_bridges::nostr_link_mode_label</c> (nostr.md § Account linking)
    /// — windows was the last app to hand-write its own copy of these three strings
    /// in <c>NostrPage.xaml.cs::BuildLinkModes</c>. Pinned against the real i18n
    /// values (not just the shared face echoing itself back) so a resw/en.yaml
    /// drift from the shared map would actually fail this.</summary>
    [Fact]
    public void NostrLinkModeLabel_ResolvesToTheThreeRealPickerStrings()
    {
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["nostr/link_account/generate"] = "Generate new keypair",
            ["nostr/link_account/import_nsec"] = "Import nsec",
            ["nostr/account/mode_remote"] = "NIP-46 bunker",
        }));

        Assert.Equal("Generate new keypair", Strings.Resolve(FaunaFfiMethods.NostrLinkModeLabel("generate")));
        Assert.Equal("Import nsec", Strings.Resolve(FaunaFfiMethods.NostrLinkModeLabel("import")));
        Assert.Equal("NIP-46 bunker", Strings.Resolve(FaunaFfiMethods.NostrLinkModeLabel("remote")));
    }

    [Fact]
    public async Task ConnectApp_SurfacesANestError_AndRevealsNothing()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();
        rpc.NextError = "boom";

        await vm.ConnectAppAsync();

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.Null(vm.ConnectString);
    }

    // ── The unified-Bridges-page tripwire ──

    [Fact]
    public async Task BridgesPage_ExcludesNostrAndBluesky_ViaTheSharedPredicate()
    {
        // A bridge with its own dedicated page must not ALSO appear as a row on the
        // unified Bridges page (nostr.md § Page structure; ui/atproto.md § Migration
        // step 2; bridges.md § Scope). apple/web/linux/android all route through the
        // shared `is_unified_bridges_page_bridge` — windows is the 5th consumer, not a
        // 5th hand-rolled `!= "nostr"`.
        //
        // Each exclusion landed WITH its page, never before it: the generic row was
        // windows's only access until the page existed. Nostr's landed 2026-07-29;
        // Bluesky's on 2026-07-31 with the Linked-account panel, which deleted the
        // temporary `NoDedicatedPageOnWindowsYet` carve-out that had kept the bluesky
        // row alive in the meantime. Both are now plain predicate consumers.
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Other("bluesky"), Nostr(), Other("activitypub") },
        };
        var vm = new BridgesViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(new[] { "activitypub" }, vm.Bridges.Select(b => b.Id));
        Assert.DoesNotContain(vm.Bridges, b => b.Id == "nostr");
        Assert.DoesNotContain(vm.Bridges, b => b.Id == "bluesky");
    }

    // ── The AT Protocol page's Linked-account panel: the same VM, scoped ──

    [Fact]
    public async Task SingleBridgeId_ScopesToThatBridge_AndSkipsTheExclusion()
    {
        // The Linked panel embeds the shared bridge card for exactly the row the
        // unified page excludes, so the scope must BYPASS the predicate — otherwise
        // the panel filters away the one bridge it exists to render. Same seam apple
        // ships as `BridgeManagerVM.singleBridgeId` (ui/atproto.md § Layout & flow
        // item 3).
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Other("bluesky"), Nostr(), Other("activitypub") },
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "bluesky" };

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(new[] { "bluesky" }, vm.Bridges.Select(b => b.Id));
    }

    [Fact]
    public async Task SingleBridgeId_SynthesizesAnUnlinkedRow_WhenTheProviderIsAbsent()
    {
        // The e2e nest is built without the bluesky provider, and a fetch can land
        // before the provider boots — in both cases `fauna.bridges.list` carries no
        // bluesky row. The panel must still render an honest UNLINKED card offering
        // the link, never vanish: a missing panel reads as a bug, and the e2e reads
        // the panel's presence off `bridge-action-button`. Same synthetic case tui's
        // `embed_bridge_card` and web's pre-fetch panel render.
        var rpc = new MockNestRpcClient { NextBridges = new[] { Other("activitypub") } };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "bluesky" };

        await vm.LoadCommand.ExecuteAsync(null);

        var row = Assert.Single(vm.Bridges);
        Assert.Equal("bluesky", row.Id);
        Assert.False(row.Linked);
        Assert.Empty(row.LinkModes);
    }

    [Fact]
    public async Task SingleBridgeId_PrefersTheRealRow_OverTheSynthetic()
    {
        // When the provider IS present the real row wins — its linked state, identity
        // and declared link modes are what the card must show.
        var realRow = new BridgeInfo("bluesky", "Bluesky", true, true, "@me.example", "oauth",
            new List<BridgeLinkMode>(), new List<BridgeSetting>());
        var rpc = new MockNestRpcClient { NextBridges = new[] { Other("activitypub"), realRow } };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "bluesky" };

        await vm.LoadCommand.ExecuteAsync(null);

        var row = Assert.Single(vm.Bridges);
        Assert.Equal("bluesky", row.Id);
        Assert.True(row.Linked);
        Assert.Equal("@me.example", row.Identity);
    }

    [Fact]
    public async Task LoadAsync_SurfacesANestError()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.False(vm.IsLoading);
    }

#if PAYMENTS
    // ── Zap signers (the NIP-57 trust root, monetization.md § Zap receipts —
    // the trust model; nostr.md § Layout & flow item 7) ──

    private static FfiFeatureRow MakeFeatureRow(
        string feature, string affordance, string? restriction = null, string? status = null) =>
        new(
            feature,
            new uniffi.fauna_core.LocalizedText("features." + feature, new Dictionary<string, string>()),
            "allow",
            null,
            Array.Empty<FfiLimitCell>(),
            null,
            null,
            "bytes",
            affordance,
            restriction is null
                ? null
                : new uniffi.fauna_core.LocalizedText(restriction, new Dictionary<string, string>()),
            new uniffi.fauna_core.LocalizedText(
                status ?? "features.status.available", new Dictionary<string, string>()));

    [Fact]
    public async Task ZapSigners_RenderForAnyLinkedAccount_UnlikeConnectedApps()
    {
        // Unlike Connected apps (custodial-only), the zap-signers roster renders
        // for ANY linked account — designating who may speak for your money is
        // orthogonal to where the key lives.
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "remote") },
        };
        rpc.ZapSignerRoster.Add(new ZapSignerEntry(1, new string('a', 64), "wallet", 100));
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(vm.ShowConnectedApps);   // remote account: the custody gate is closed
        Assert.Single(vm.ZapSigners);         // zap signers: rendered anyway
    }

    [Fact]
    public async Task ZapSigners_HiddenWhileUnlinked()
    {
        var rpc = new MockNestRpcClient { NextBridges = new[] { Nostr() } };
        rpc.ZapSignerRoster.Add(new ZapSignerEntry(1, new string('a', 64), "wallet", 100));
        var vm = new NostrViewModel(rpc);

        await vm.LoadAsync();

        Assert.Empty(vm.ZapSigners);   // never even fetched while unlinked
    }

    [Fact]
    public async Task AddZapSigner_RendersTheStoredPubkey_NotTheTypedInput()
    {
        // The nest normalizes to lowercase on write; only that form ever matches
        // a real receipt.
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();
        var typed = new string('A', 64);

        await vm.AddZapSignerAsync(typed, "my wallet");

        var row = Assert.Single(vm.ZapSigners);
        Assert.Equal(typed.ToLowerInvariant(), row.SignerPubkey);
        Assert.Contains(FaunaFfiMethods.ShortId(typed.ToLowerInvariant()), row.DisplayText);
        Assert.DoesNotContain(typed, row.DisplayText);   // never the uppercase input
    }

    [Fact]
    public async Task AddZapSigner_RejectsANon64HexPubkey_ClientSide()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();

        await vm.AddZapSignerAsync("not-hex", "label");

        Assert.DoesNotContain("NostrZapSignersAdd", rpc.Calls);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.Empty(vm.ZapSigners);
    }

    [Fact]
    public async Task RemoveZapSigner_IsNeverGated_EvenWhenAddIsDenied()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
            NextFeatureRows = new[] { MakeFeatureRow("zaps", "disabled", status: "restricted") },
        };
        rpc.ZapSignerRoster.Add(new ZapSignerEntry(1, new string('a', 64), "wallet", 100));
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();
        Assert.NotNull(vm.ZapSignerAddGateReason);   // add is denied

        await vm.RemoveZapSignerAsync(0);

        Assert.Contains("NostrZapSignersRemove", rpc.Calls);
        Assert.Empty(vm.ZapSigners);   // removal went through regardless of the add gate
    }

    [Fact]
    public async Task ZapSignerAddGateReason_NeverDisablesEagerly_BeforeTheFetchResolves()
    {
        // A failed features fetch must leave the button live — the nest, not the
        // app, is the enforcement floor.
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Nostr(linked: true, mode: "generated") },
        };
        var vm = new NostrViewModel(rpc);
        await vm.LoadAsync();
        rpc.NextError = "boom";

        await vm.AddZapSignerAsync(new string('a', 64), "label");   // triggers a re-refresh

        Assert.Null(vm.ZapSignerAddGateReason);
    }

    [Fact]
    public void ZapSignerAddGateReasonFrom_ReadsOffTheRowsAffordance_NeverReDerived()
    {
        Assert.Null(NostrViewModel.ZapSignerAddGateReasonFrom(
            new[] { MakeFeatureRow("zaps", "available") }));
        Assert.Null(NostrViewModel.ZapSignerAddGateReasonFrom(
            Array.Empty<FfiFeatureRow>()));   // no row at all -> the button stays live
        // A `hidden` affordance still disables — the excision story is the
        // orthogonal compile-time cargo feature, never this render-time gate.
        Assert.NotNull(NostrViewModel.ZapSignerAddGateReasonFrom(
            new[] { MakeFeatureRow("zaps", "hidden", status: "restricted") }));
        Assert.NotNull(NostrViewModel.ZapSignerAddGateReasonFrom(
            new[] { MakeFeatureRow("zaps", "disabled", status: "restricted") }));
    }

    [Fact]
    public void ZapSignerRow_FallsBackToUnnamed_AndRendersTheShortId()
    {
        var unnamed = new ZapSignerEntry(1, new string('a', 64), string.Empty, 100);
        var named = new ZapSignerEntry(2, new string('b', 64), "My wallet", 100);

        Assert.Contains(Strings.Get("nostr/zap_signers/unnamed"), ZapSignerRow.From(unnamed).DisplayText);
        Assert.Contains("My wallet", ZapSignerRow.From(named).DisplayText);
        Assert.Contains(FaunaFfiMethods.ShortId(named.SignerPubkey), ZapSignerRow.From(named).DisplayText);
    }
#endif
}
