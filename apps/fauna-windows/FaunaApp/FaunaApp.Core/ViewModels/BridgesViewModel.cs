using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

public partial class BridgesViewModel : ViewModelBase
{
    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _selectedBridgeId;

    public ObservableCollection<BridgeInfo> Bridges { get; } = new();
    public ObservableCollection<BridgeFollow> Follows { get; } = new();
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
                if (keep) Bridges.Add(b);
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

    // ── Bridge Follows ──

    [RelayCommand]
    private async Task LoadFollowsAsync(string bridgeId)
    {
        SelectedBridgeId = bridgeId;
        ErrorMessage = null;
        try
        {
            Follows.Clear();
            foreach (var f in await _rpc.BridgesListFollowsAsync(bridgeId))
                Follows.Add(f);
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    private async Task AddFollowAsync(string idAndPetname)
    {
        if (SelectedBridgeId is null) return;
        var parts = idAndPetname.Split('|', 2);
        var id = parts[0].Trim();
        var petname = parts.Length > 1 ? parts[1].Trim() : null;
        ErrorMessage = null;
        try
        {
            await _rpc.BridgesAddFollowAsync(SelectedBridgeId, id, petname);
            await LoadFollowsAsync(SelectedBridgeId);
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    private async Task RemoveFollowAsync(string followId)
    {
        if (SelectedBridgeId is null) return;
        ErrorMessage = null;
        try
        {
            await _rpc.BridgesRemoveFollowAsync(SelectedBridgeId, followId);
            await LoadFollowsAsync(SelectedBridgeId);
        }
        catch (Exception ex) { ShowError(ex); }
    }

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
