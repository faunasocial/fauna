using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

public partial class BridgesViewModel : ViewModelBase
{
    [ObservableProperty] private bool _isLoading;

    public ObservableCollection<BridgeInfo> Bridges { get; } = new();
    public ObservableCollection<BridgeFeedSubscription> Subscriptions { get; } = new();

    /// <summary>When set, <see cref="LoadAsync"/> scopes <see cref="Bridges"/> to just
    /// this one bridge id and SKIPS the unified-page exclusion filter — the Bluesky
    /// page's Linked-account panel uses it to focus the <c>"bluesky"</c> row that
    /// <c>is_unified_bridges_page_bridge</c> deliberately keeps off the generic list
    /// (<c>docs/goal/ui/atproto.md</c> § Migration). <c>null</c> (the default) leaves
    /// the unified Bridges page's behavior untouched.
    ///
    /// <para>Same seam apple ships (<c>BridgeManagerVM.singleBridgeId</c>) and the same
    /// shape android uses (a second <c>BridgesVM</c> instance reading the identical
    /// unfiltered reply) — one concept on every app rather than a per-app fetch
    /// (priority #1/#3).</para></summary>
    public string? SingleBridgeId { get; set; }

    private readonly INestRpcClient _rpc;

    internal BridgesViewModel(INestRpcClient rpc) { _rpc = rpc; }

    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ErrorMessage = null;
        Bridges.Clear();
        try
        {
            // A bridge with its OWN dedicated settings page is excluded from the
            // unified Bridges page (bridges.md § Scope; nostr.md § Page structure,
            // ratified 2026-06-13 — Nostr gets the same treatment as mail). The rule
            // is shared Rust (`fauna_client_bridges::is_unified_bridges_page_bridge`)
            // rather than a 5th per-app literal comparison — apple/web/linux/android
            // all de-duplicated onto it, and windows is the 5th consumer.
            //
            // The filter runs HERE, at the page, never at the fetch: linux, tui and
            // android each paid for that mistake, android's instance silently hiding
            // the Bluesky row with no page to send it to. `BridgesListAsync` stays
            // unfiltered, so NostrViewModel, StatusViewModel and the AT Protocol page's
            // single-bridge scope below all read the same complete reply.
            foreach (var b in await _rpc.BridgesListAsync())
            {
                var keep = SingleBridgeId is { } only
                    ? b.Id == only
                    : FaunaFfiMethods.IsUnifiedBridgesPageBridge(b.Id);
                if (keep) Bridges.Add(await WithCardReadsAsync(b));
            }

            // A single-bridge scope always yields exactly one card. When the provider
            // has no row at all — it is not compiled into this nest, or the fetch has
            // not landed — render a synthetic UNLINKED one rather than nothing: the
            // panel's job is to offer the link, and a vanished panel reads as a bug.
            // Same honest-empty-surface case tui's `embed_bridge_card` synthesizes and
            // web's pre-fetch panel renders.
            if (SingleBridgeId is { } id && Bridges.Count == 0)
            {
                Bridges.Add(new BridgeInfo(
                    Id: id,
                    DisplayName: id,
                    Available: true,
                    Linked: false,
                    Identity: null,
                    Mode: null,
                    LinkModes: Array.Empty<BridgeLinkMode>(),
                    Settings: Array.Empty<BridgeSetting>()));
            }
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Link a bridge in <paramref name="mode"/> with the dialog's
    /// per-mode <paramref name="fields"/> (composed into the <c>params</c> CBOR
    /// map in <see cref="INestRpcClient.BridgesLinkAsync"/>).</summary>
    public async Task LinkBridgeAsync(string bridgeId, string mode, IReadOnlyDictionary<string, string> fields)
    {
        ErrorMessage = null;
        try
        {
            // FAUNA_E2E_AGENT_LOG ceremony trace: the link RPC rides a 30 s
            // protocol envelope (kind.rs `fauna.bridges.link`) inside which the
            // client silently waits out reconnects — a dispatched/ok/FAILED
            // triplet is what tells a slow RPC from one that never went out.
            Logs.E2eTrace.Write($"[bridge-link] rpc dispatching ({bridgeId}, mode={mode})");
            await _rpc.BridgesLinkAsync(bridgeId, mode, fields);
            Logs.E2eTrace.Write($"[bridge-link] rpc ok ({bridgeId})");
            await LoadAsync();
        }
        catch (Exception ex)
        {
            Logs.E2eTrace.Write(
                $"[bridge-link] rpc FAILED ({bridgeId}): {ex.GetType().Name}: {ex.Message}");
            ShowSourceRefusalOrError(
                ex, bridgeId, FfiFeedSourceOperation.Link, target: string.Empty);
        }
    }

    // ── The ward's feed-source ask (family-safety.md § Feed-source approvals → App
    //    affordance; tui's bridges.rs `guardian_refused` / `source_ask_rows`) ──

    /// <summary>Show a failed link / follow. The nest's TYPED guardian refusal
    /// (<c>FfiException.GuardianApprovalRequired</c> — never message text, rule (a))
    /// is recorded as the <c>(bridge_id, operation, target)</c> triple that was refused,
    /// which is what makes <c>bridge-source-request-button</c> appear on that bridge's
    /// card; it still STAYS on <c>error-message</c> (rule (b)) with the feed-source
    /// sentence, since the operation genuinely did not happen. Every other failure takes
    /// the ordinary path. Keyed on the ask data, never on the add-follow form's buffers
    /// (rule (d)).</summary>
    private void ShowSourceRefusalOrError(
        Exception ex, string bridgeId, FfiFeedSourceOperation operation, string target)
    {
        if (ex is FfiException.GuardianApprovalRequired)
        {
            WardAsksCache.NoteFeedRefusal(new FeedTriple(
                bridgeId, FaunaFfiMethods.FeedSourceOperationWire(operation), target));
            SetError(Strings.Get("bridges/source_blocked"));
        }
        else
        {
            ShowError(ex);
        }
    }

    /// <summary><c>bridge-source-request-button</c> — ask the guardian to approve the
    /// refused <paramref name="triple"/> (<c>fauna.family.feed_source.request</c>).
    /// <paramref name="label"/> is display-only, never authorizing (the bridge's name,
    /// the one label still available once the add-follow buffers are cleared). A
    /// failure surfaces on the page's error bar; success needs no local flag — the
    /// shared status re-read lands the durable row the card renders.</summary>
    internal async Task AskSourceAsync(FeedTriple triple, string label)
    {
        try
        {
            await _rpc.FamilyFeedSourceRequestAsync(
                triple.BridgeId, triple.Operation, triple.Target, label);
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Write one metadata-driven <c>BridgeSetting</c> row (bool/text/number —
    /// bridges.md § Bridge settings) and re-read, mirroring
    /// Link/Unlink's own re-read-after-write shape on this VM (richer than the
    /// narrower <see cref="NostrViewModel"/> convenience, since it re-syncs the whole
    /// list rather than one bridge's snapshot). Nest-side clamping means the value
    /// this returns can differ from what was sent — the re-read is what shows the
    /// clamped truth rather than the client's own optimistic guess.</summary>
    public async Task SetSettingAsync(string bridgeId, BridgeSettingValue value)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.BridgesSetSettingsAsync(bridgeId, new[] { value });
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    [RelayCommand]
    private async Task UnlinkBridgeAsync(string bridgeId)
    {
        ErrorMessage = null;
        try
        {
            Logs.E2eTrace.Write($"[bridge-unlink] rpc dispatching ({bridgeId})");
            await _rpc.BridgesUnlinkAsync(bridgeId);
            Logs.E2eTrace.Write($"[bridge-unlink] rpc ok ({bridgeId})");
            await LoadAsync();
        }
        catch (Exception ex)
        {
            Logs.E2eTrace.Write(
                $"[bridge-unlink] rpc FAILED ({bridgeId}): {ex.GetType().Name}: {ex.Message}");
            ShowError(ex);
        }
    }

    // ── Follow requests (bridges.md § Follow requests) ──

    /// <summary>The per-bridge lists a linked card paints — its follows (bridges.md
    /// § Follows) and the follow requests waiting on it (§ Follow requests) — read with
    /// the list, each only when the provider declares it, so the card paints them with
    /// the rest of its row (apple's <c>BridgeManagerVM.refresh</c> reads both in the same
    /// pass). A failed read shows on <c>error-message</c> and the card paints that
    /// section with no rows.</summary>
    private async Task<BridgeInfo> WithCardReadsAsync(BridgeInfo bridge)
    {
        if (!bridge.Linked) return bridge;
        try
        {
            if (bridge.SupportsFollows)
                bridge = bridge with { Follows = await _rpc.BridgesListFollowsAsync(bridge.Id) };
            if (bridge.SupportsFollowRequests)
                bridge = bridge with { FollowRequests = await _rpc.BridgesListFollowRequestsAsync(bridge.Id) };
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        return bridge;
    }

    /// <summary><c>bridge-follow-request-approve</c> / <c>-refuse</c>: answer one request
    /// by the id its row carries, then re-read — non-optimistic, no confirm (bridges.md
    /// § Follow requests → Layout, Where logic lives).</summary>
    public async Task AnswerFollowRequestAsync(string bridgeId, string id, bool approve)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.BridgesAnswerFollowRequestAsync(bridgeId, id, approve);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    // ── Bridge Follows (bridges.md § Follows, § User actions) ──
    //
    // Per bridge, off the card's own row: the follows ride BridgeInfo.Follows (read in
    // WithCardReadsAsync), and each write re-reads the list — non-optimistic, like the
    // settings and the follow requests.

    /// <summary><c>bridge-add-follow-button</c>: follow <paramref name="id"/> (the external
    /// id; ActivityPub's is the remote actor URL) with an optional nickname on
    /// <paramref name="bridgeId"/>, then re-read. A typed guardian refusal is recorded
    /// as that follow's own triple, never the form's buffers (rule (d)).</summary>
    public async Task AddFollowAsync(string bridgeId, string id, string? petname)
    {
        id = id.Trim();
        if (id.Length == 0) return;
        petname = string.IsNullOrWhiteSpace(petname) ? null : petname.Trim();
        ErrorMessage = null;
        try
        {
            await _rpc.BridgesAddFollowAsync(bridgeId, id, petname);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ShowSourceRefusalOrError(ex, bridgeId, FfiFeedSourceOperation.Follow, id);
        }
    }

    /// <summary><c>bridge-follow-remove</c>: unfollow one row's follow (idempotent), then
    /// re-read.</summary>
    public async Task RemoveFollowAsync(string bridgeId, string followId)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.BridgesRemoveFollowAsync(bridgeId, followId);
            await LoadAsync();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    // ── The admin's hosted plugins (bridges.md § Hosted plugins on this page;
    //    third-party.md § The runner contract → The install-approval leg). Reference
    //    painters: tui `apps/fauna-tui/src/bridges.rs::plugin_elements`, android
    //    `BridgesScreen.kt::PluginsSection`. The rows arrive paint-ready from shared Rust
    //    (`installed_plugin_text`); this layer only resolves their texts. ──

    /// <summary>Whether the signed-in account is this nest's admin
    /// (<c>fauna.account.am_i_admin</c>, read by <see cref="LoadPluginsAsync"/>). Fail
    /// closed: a failed read leaves it false, so the admin section stays hidden — the
    /// same gate the Admin nav entry reads.</summary>
    public bool IsAdmin { get; private set; }

    /// <summary>The admin's installed plugins — <c>null</c> until a read has returned, so
    /// the page paints neither rows nor the empty line before it (the three-state list,
    /// <c>ui/README.md</c> § List pages).</summary>
    internal IReadOnlyList<FfiInstalledPlugin>? Plugins { get; private set; }

    /// <summary>True after an install opened its card: the page shows the
    /// <c>bridges.plugin_install_opened</c> hint until the next install starts.</summary>
    public bool PluginInstallOpened { get; private set; }

    /// <summary>The plugin whose inline uninstall confirm is open, by install row id.</summary>
    public string? PluginUninstallArmed { get; private set; }

    /// <summary>Read the admin gate, then — for an admin only, since only an admin may
    /// list — the installed plugins. A failed list shows on <c>error-message</c> and
    /// leaves the list unread.</summary>
    public async Task LoadPluginsAsync()
    {
        try
        {
            IsAdmin = await _rpc.AmIAdminAsync();
        }
        catch (Exception)
        {
            IsAdmin = false;
        }
        if (!IsAdmin)
        {
            Plugins = null;
            return;
        }
        try
        {
            Plugins = await _rpc.PluginsListAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary><c>bridge-plugin-install-submit</c> — ADMIN: <c>fauna.plugins.install</c>
    /// with the typed document URL. The nest opens the install card in Connected apps →
    /// Requests, which the admin answers like any card; the page then shows the hint.
    /// Returns whether the install was accepted, so the page clears the field only
    /// then.</summary>
    public async Task<bool> InstallPluginAsync(string documentUrl)
    {
        var url = documentUrl.Trim();
        if (url.Length == 0) return false;
        ErrorMessage = null;
        PluginInstallOpened = false;
        try
        {
            await _rpc.PluginsInstallAsync(url);
            PluginInstallOpened = true;
            return true;
        }
        catch (Exception ex)
        {
            ShowError(ex);
            return false;
        }
    }

    /// <summary><c>bridge-plugin-uninstall</c> — the first press arms the row's inline
    /// confirm; a press on the armed row runs <c>fauna.plugins.uninstall</c> and re-reads
    /// the list (non-optimistic).</summary>
    public async Task PressUninstallPluginAsync(string principalIdHex)
    {
        if (PluginUninstallArmed != principalIdHex)
        {
            PluginUninstallArmed = principalIdHex;
            return;
        }
        PluginUninstallArmed = null;
        ErrorMessage = null;
        try
        {
            await _rpc.PluginsUninstallAsync(principalIdHex);
        }
        catch (Exception ex)
        {
            ShowError(ex);
            return;
        }
        await LoadPluginsAsync();
    }

    /// <summary>The untagged <i>Keep</i> beside an armed confirm.</summary>
    public void CancelUninstallPlugin() => PluginUninstallArmed = null;

    /// <summary>The <c>bridge-plugin-publisher</c> text: the shared row's publisher line,
    /// resolved.</summary>
    internal static string PluginPublisherText(FfiInstalledPlugin row) => Strings.Resolve(row.publisher);

    /// <summary>The <c>bridge-plugin-status</c> text: the shared row's status lines, top to
    /// bottom, resolved and joined.</summary>
    internal static string PluginStatusText(FfiInstalledPlugin row)
    {
        var lines = row.status;
        return string.Join("\n", lines.Select(Strings.Resolve));
    }

    // ── Every account's plugin settings cards (bridges.md § Hosted plugins on this page;
    //    third-party.md § The principal model → Hosted principals). One more `bridge-card`
    //    per installed plugin, composed once in shared Rust (`plugin_card_text`); Connect and
    //    Disconnect are the connected-apps machine's `bind_plugin` / `revoke_principal`, so
    //    the bind card waits in Connected apps → Requests, the one approve path. Reference
    //    painters: tui `bridges.rs::plugin_card_elements`, android
    //    `BridgesScreen.kt::PluginCard` / `BridgesVM.pressPluginCard`. ──

    private readonly Func<Task<uniffi.fauna_client_connected_apps.IConnectedAppsMachine>>? _cardMachineFactory;
    private uniffi.fauna_client_connected_apps.IConnectedAppsMachine? _cardMachine;

    /// <summary>Test seam: the connected-apps machine the plugin cards' verbs go through
    /// (the mock seam cannot build a real one).</summary>
    internal BridgesViewModel(
        INestRpcClient rpc,
        Func<Task<uniffi.fauna_client_connected_apps.IConnectedAppsMachine>> cardMachineFactory)
        : this(rpc)
    {
        _cardMachineFactory = cardMachineFactory;
    }

    /// <summary>The account's plugin settings cards — <c>null</c> until the read returned
    /// (the page paints none before it).</summary>
    internal IReadOnlyList<FfiPluginCard>? PluginCards { get; private set; }

    /// <summary>True after a Connect opened the bind card: the page shows the
    /// <c>bridges.plugin_bind_opened</c> hint until the next press.</summary>
    public bool PluginBindOpened { get; private set; }

    /// <summary><c>fauna.plugins.available</c> for this account. A failed read shows on
    /// <c>error-message</c> and leaves the cards as they were.</summary>
    public async Task LoadPluginCardsAsync()
    {
        try
        {
            PluginCards = await _rpc.PluginsAvailableAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>The card's <c>bridge-action-button</c>: <i>Connect</i> while unbound
    /// (<c>bind_plugin</c> on the install row — the bind card then waits in Connected apps →
    /// Requests), <i>Disconnect</i> while bound (<c>revoke_principal</c> on this account's
    /// binding row). The machine's page error, when it set one, is the failure; then the
    /// cards are re-read (non-optimistic).</summary>
    internal async Task PressPluginCardAsync(FfiPluginCard card)
    {
        ErrorMessage = null;
        PluginBindOpened = false;
        try
        {
            var machine = _cardMachine ??= await BuildCardMachineAsync();
            if (card.bound && card.bindingPrincipalIdHex is { } binding)
                await machine.RevokePrincipal(Convert.FromHexString(binding));
            else
                await machine.BindPlugin(Convert.FromHexString(card.principalIdHex));
            if (machine.Snapshot().error is { } failed)
            {
                SetError(Strings.Resolve(failed));
                return;
            }
            if (!card.bound) PluginBindOpened = true;
        }
        catch (Exception ex)
        {
            ShowError(ex);
            return;
        }
        await LoadPluginCardsAsync();
    }

    private Task<uniffi.fauna_client_connected_apps.IConnectedAppsMachine> BuildCardMachineAsync() =>
        _cardMachineFactory is { } factory
            ? factory()
            // The bridges page paints the cards from its own re-read, so the machine's
            // change notifications have no listener here.
            : _rpc.BuildConnectedAppsMachineAsync(new UnobservedConnectedApps(), null);

    /// <summary>A plugin card's settings row committed a value: <c>fauna.plugins.set_settings</c>
    /// for this account's own setting, then the cards are re-read (nest-side clamping means
    /// the re-read shows the truth, not the typed value).</summary>
    internal async Task SetPluginSettingAsync(string principalIdHex, BridgeSettingValue value)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.PluginsSetSettingAsync(principalIdHex, value);
        }
        catch (Exception ex)
        {
            ShowError(ex);
            return;
        }
        await LoadPluginCardsAsync();
    }

    /// <summary>The card's settings as the bridge cards' own rows, for the one settings
    /// renderer. Unbound there are none.</summary>
    internal static IReadOnlyList<BridgeSetting> PluginCardSettings(FfiPluginCard card) =>
        card.settings.Select(BridgeSetting.FromFfi).ToList();

    /// <summary>The card's badge and publisher lines, resolved.</summary>
    internal static string PluginCardSubtitle(FfiPluginCard card) =>
        $"{Strings.Resolve(card.badge)}\n{Strings.Resolve(card.publisher)}";

    /// <summary>The <c>bridge-action-button</c> label: <i>Connect</i> or <i>Disconnect</i>.</summary>
    internal static string PluginCardActionLabel(FfiPluginCard card) =>
        Strings.Get(card.bound ? "bridges/plugin_disconnect" : "bridges/plugin_connect");

    /// <summary>The <c>bridge-action-button</c>'s attributes, packed into the one HelpText UIA
    /// gives it: the plugin's <c>client_id</c> (which card is which plugin's) and <c>bound</c>
    /// (<c>true</c>/<c>false</c>).</summary>
    internal static string PluginCardActionAttributes(FfiPluginCard card) =>
        FaunaApp.Core.Helpers.AutomationAttributePack.Pack(
            ("client_id", card.clientId),
            ("bound", card.bound ? "true" : "false"));

    // ── Bridge Feed Subscriptions ──

    [RelayCommand]
    private async Task LoadSubscriptionsAsync()
    {
        ErrorMessage = null;
        try
        {
            Subscriptions.Clear();
            foreach (var s in await _rpc.BridgesFeedsListAsync())
                Subscriptions.Add(s);
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    private async Task SubscribeFeedAsync(string bridgeAndUriAndName)
    {
        var parts = bridgeAndUriAndName.Split('|', 3);
        if (parts.Length != 3) return;
        ErrorMessage = null;
        try
        {
            await _rpc.BridgesFeedsCreateAsync(parts[0].Trim(), parts[1].Trim(), parts[2].Trim());
            await LoadSubscriptionsAsync();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    private async Task UnsubscribeFeedAsync(long id)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.BridgesFeedsDeleteAsync(id);
            await LoadSubscriptionsAsync();
        }
        catch (Exception ex) { ShowError(ex); }
    }
}
