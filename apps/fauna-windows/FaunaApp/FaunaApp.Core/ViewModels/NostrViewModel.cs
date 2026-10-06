using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The standalone Nostr settings page (docs/goal/ui/nostr.md § Page structure —
/// ratified 2026-06-13: Nostr keeps its OWN dedicated page, the same treatment as
/// mail, NOT folded into the unified Bridges page). Windows was the last client
/// with no Nostr surface at all (§ Implementation status today).
///
/// <para>The control plane rides the unified <c>fauna.bridges.*</c> WS-RPC keyed
/// <c>bridge_id:"nostr"</c> (§ WS-RPC migration contract) — so there is no new FFI
/// and no new Rust here, only a call-site consumer of the seam windows already had.
/// Every rule this page must not re-break is pinned by a test in
/// <c>NostrViewModelTests</c>:</para>
/// <list type="bullet">
/// <item><b><see cref="Registered"/> gates the unavailable notice — <see cref="Available"/>
/// gates NOTHING.</b> <c>NostrProvider::available</c> is the nsec-deposit bootstrap gate
/// (§ The bridging gate), correctly <c>false</c> on a box where nobody has deposited a
/// key yet. web and apple both once conflated it with "the bridge exists", hiding the
/// account-link form — the ONLY thing that can bootstrap the first deposit — behind it,
/// permanently (fixed 2026-07-14; nostr.md § Implementation status today). windows was
/// never audited for that conflation because it had no page; this builds it correct.</item>
/// <item><b>The link gate actually swaps the surface.</b> linux shipped this page with
/// every row rendering unconditionally, so <c>nostr-pubkey-copy-btn</c> — the e2e
/// <c>is_linked()</c> signal — was visible while unlinked and silently no-opped two
/// downstream tests (fixed 2026-07-19).</item>
/// <item><b>Non-optimistic.</b> Every mutation re-reads <c>fauna.bridges.list</c> before
/// the new state is published, so a toggle shows what the nest persisted, never the tap.</item>
/// </list>
///
/// <para>All Nostr logic stays in shared Rust (priority #2): the relay-URL refusal and its
/// message are <c>fauna_protocol::nostr_relay::relay_url_error</c> via its UniFFI face, and the
/// signing-mode label is <c>fauna_client_bridges::nostr_key_source_label</c> — never a
/// per-app prefix check or enum ternary (§ Where logic lives).</para>
/// </summary>
public partial class NostrViewModel : ViewModelBase
{
    public const string BridgeId = "nostr";

    // Link-request modes. Native apps offer generate/import/remote only — NIP-07 is
    // a browser-extension boundary and is web-only (nostr.md § Architectural rules #4),
    // matching apple/android/linux/tui.
    public const string ModeGenerate = "generate";
    public const string ModeImport = "import";
    public const string ModeRemote = "remote";

    private const string RelayListKey = "relay_list";

    private readonly INestRpcClient _rpc;

    // internal, matching every sibling page VM: INestRpcClient is itself internal
    // (the test + WinUI assemblies see it via [InternalsVisibleTo]).
    internal NostrViewModel(INestRpcClient rpc) { _rpc = rpc; }

    // ── Bridge state ──

    [ObservableProperty] private bool _isLoading;

    /// <summary>The nostr bridge appears in <c>fauna.bridges.list</c> at all — false only
    /// when the nest was built without the <c>nostr</c> cargo feature. This is the ONLY
    /// thing that may gate the "unavailable" notice.</summary>
    [ObservableProperty] private bool _registered;

    /// <summary>The nsec-deposit bridging gate (<c>nostr_bridging_available</c>). Surfaced
    /// for display only — deliberately gates nothing, see the class remarks.</summary>
    [ObservableProperty] private bool _available;

    [ObservableProperty] private bool _linked;
    [ObservableProperty] private string? _identityDisplay;
    [ObservableProperty] private string? _signingMode;

    /// <summary>The succession-aftermath npub-confirm banner
    /// (<c>nostr.md</c> § Key succession and rotation, leg 3) — true only
    /// while a successor has not yet confirmed the currently-linked npub
    /// since their most recent identity succession. Checked on every load,
    /// same as tui's nav-enter leg (<c>refresh_and_check_npub</c>).</summary>
    [ObservableProperty] private bool _npubConfirmationOwed;

    /// <summary>The selected link-request mode while unlinked (the picker's value).</summary>
    [ObservableProperty] private string _linkMode = ModeGenerate;

    public ObservableCollection<NostrRelayRow> Relays { get; } = new();
    public ObservableCollection<BridgeFollow> Follows { get; } = new();

    /// <summary>The one-time <c>bunker://…</c> reveal from the most recent mint
    /// (<c>nostr-bunker-connect-string</c>), or null when nothing is being revealed.
    /// The nest never serves this string again — the secret inside it is single-use
    /// with a hard-coded TTL — so it is view state only, deliberately never
    /// persisted and dropped on the next refresh of the surface that owns it.</summary>
    [ObservableProperty] private string? _connectString;

#if PAYMENTS
    /// <summary>The *Zap signers* roster (<c>nostr-zap-signer-item</c>) — the
    /// NIP-57 trust root (monetization.md § Zap receipts — the trust model).
    /// Rendered for ANY linked account, unlike the connect invite:
    /// designating who may speak for your money is orthogonal to where the
    /// key lives (nostr.md § Layout &amp; flow item 7).</summary>
    public ObservableCollection<ZapSignerRow> ZapSigners { get; } = new();

    /// <summary>Why <c>nostr-zap-signer-add-btn</c> is disabled, or null when
    /// it works — the Dim-3 courtesy read off the shared feature-row plane.
    /// Never disabled eagerly: an un-hydrated read leaves the button live
    /// (the nest, not the app, is the enforcement floor).</summary>
    [ObservableProperty] private string? _zapSignerAddGateReason;
#endif

    private readonly Dictionary<string, bool> _toggles = new();

    /// <summary>The localized signing-mode label for the stored mode the bridge reports
    /// (generated / imported / remote / nip07 / proxied) — single-sourced in shared Rust
    /// so all 7 apps render the same string (nostr.md § User actions → Signing-mode
    /// display). Empty while unlinked.</summary>
    public string SigningModeLabel => string.IsNullOrEmpty(SigningMode)
        ? string.Empty
        : Strings.Resolve(FaunaFfiMethods.NostrKeySourceLabel(SigningMode!));

    /// <summary>The npub-confirm banner's own self-contained text — "the
    /// public key shown above ({npub}) is yours", not just a pointer at the
    /// row above (i18n comment on <c>nostr/npub_confirm/banner</c>).</summary>
    public string NpubConfirmBannerText =>
        Strings.Get("nostr/npub_confirm/banner").Replace("{npub}", IdentityDisplay ?? "—");

    /// <summary>Connected apps (NIP-46 bunker) render only for a linked account in a
    /// custodial mode — a <c>remote</c> account keeps its key off this box, so the box
    /// can never itself be that account's signer (nostr.md § Layout &amp; flow item 6).
    /// </summary>
    public bool ShowConnectedApps =>
        Linked && SigningMode is "generated" or "imported";

    /// <summary>A content toggle's persisted value (false when absent).</summary>
    public bool Toggle(string key) => _toggles.TryGetValue(key, out var v) && v;

    // ── Load ──

    [RelayCommand]
    public async Task LoadAsync()
    {
        IsLoading = true;
        SetError(null);
        try
        {
            await RefreshAsync();
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

    /// <summary>Re-read the authoritative bridge row and re-project every derived
    /// surface. Every mutation ends here — that is what makes the page non-optimistic.
    /// Throws; callers wrap.</summary>
    private async Task RefreshAsync()
    {
        var bridges = await _rpc.BridgesListAsync();
        var nostr = bridges.FirstOrDefault(b => b.Id == BridgeId);

        Registered = nostr is not null;
        Available = nostr?.Available ?? false;
        Linked = nostr?.Linked ?? false;
        IdentityDisplay = nostr?.Identity;
        SigningMode = nostr?.Mode;
        OnPropertyChanged(nameof(SigningModeLabel));
        OnPropertyChanged(nameof(ShowConnectedApps));
        OnPropertyChanged(nameof(NpubConfirmBannerText));

        // Best-effort like the shared predicate itself (nostr_npub_confirm.rs
        // module docs): a bad read never turns into a page error, it is just
        // re-asked on the next load — the same courtesy ZapSignerAddGateReason
        // below takes for its own Dim-3 read.
        try
        {
            NpubConfirmationOwed = await _rpc.NpubConfirmationOwedAsync();
        }
        catch
        {
            NpubConfirmationOwed = false;
        }

        // Keys, ids, labels and defaults all come from the shared catalog
        // (nostr.md § Where logic lives → *The content-toggle catalog*), none
        // re-spelled here. An unreported key falls back to the catalog's own
        // `default_on` — the NEST's default — rather than a client-side
        // `false`, so a never-configured account paints what the nest would
        // actually do.
        _toggles.Clear();
        foreach (var opt in FaunaFfiMethods.NostrContentToggleOptions())
        {
            var s = nostr?.Settings.FirstOrDefault(x => x.Key == opt.@key);
            _toggles[opt.@key] = s?.BoolValue ?? opt.@defaultOn;
        }

        Relays.Clear();
        foreach (var url in ParseRelayList(RelayListJson(nostr)))
            Relays.Add(new NostrRelayRow(url));

        Follows.Clear();
        // A follow roster only exists for a linked account; asking otherwise is a
        // guaranteed provider error on an unlinked bridge.
        if (Linked)
        {
            foreach (var f in await _rpc.BridgesListFollowsAsync(BridgeId))
                Follows.Add(f);
        }

#if PAYMENTS
        // Rendered for ANY linked account (not custodial-gated — see the class
        // remarks on ZapSigners).
        ZapSigners.Clear();
        if (Linked)
        {
            foreach (var s in await _rpc.NostrZapSignersListAsync())
                ZapSigners.Add(ZapSignerRow.From(s));
        }

        // The Dim-3 courtesy gate. A failed fetch leaves the prior verdict (or
        // null) rather than surfacing a page-level error over a courtesy
        // affordance the nest itself still enforces — apple's
        // refreshZapSignerGate shape.
        if (Linked)
        {
            try
            {
                var rows = await _rpc.FeaturesRowsAsync();
                ZapSignerAddGateReason = ZapSignerAddGateReasonFrom(rows);
            }
            catch
            {
                // leave the prior verdict
            }
        }
        else
        {
            ZapSignerAddGateReason = null;
        }
#endif
    }

#if PAYMENTS
    /// <summary>Why <c>nostr-zap-signer-add-btn</c> is disabled, or null when it
    /// works. Mirrors apple's <c>NostrVM.zapSignerAddGateReason</c> / linux's
    /// <c>zap_signer_add_gate_reason</c> / tui's <c>designate_gate</c> exactly:
    /// the decision is READ off the row's own <c>Affordance</c>, never
    /// re-derived — a <c>hidden</c> affordance still disables here exactly like
    /// <c>disabled</c> does (the *removal* story is the orthogonal compile-time
    /// <c>zaps</c> cargo feature, not this render-time courtesy).</summary>
    internal static string? ZapSignerAddGateReasonFrom(IReadOnlyList<FfiFeatureRow> rows)
    {
        var row = rows.FirstOrDefault(r => r.@feature == "zaps");
        if (row is null || row.@affordance == "available") return null;
        return row.@restriction is { } restriction
            ? Strings.ResolveNested(restriction)
            : Strings.Resolve(row.@status);
    }
#endif

    private static string RelayListJson(BridgeInfo? nostr) =>
        nostr?.Settings.FirstOrDefault(s => s.Key == RelayListKey)?.TextValue ?? string.Empty;

    /// <summary>The <c>relay_list</c> setting is a JSON array of URL strings carried in a
    /// CBOR <i>text</i> value (<c>bridge_provider.rs</c> — <c>Value::String</c>), so it is
    /// parsed, not read directly. Malformed or absent JSON yields an empty list rather
    /// than throwing: a settings blob a future nest grew must never break the page.</summary>
    internal static IReadOnlyList<string> ParseRelayList(string? json)
    {
        if (string.IsNullOrWhiteSpace(json)) return Array.Empty<string>();
        try
        {
            return JsonSerializer.Deserialize<List<string>>(json!) ?? new List<string>();
        }
        catch (JsonException)
        {
            return Array.Empty<string>();
        }
    }

    // ── Account linking ──

    /// <summary>Link the account in <see cref="LinkMode"/>. <paramref name="secret"/>
    /// carries the pasted nsec in import mode and the bunker URL in remote mode; it is
    /// ignored for generate. The field names match the provider's declared link fields.
    /// </summary>
    [RelayCommand]
    public async Task LinkAsync(string? secret)
    {
        SetError(null);
        var fields = new Dictionary<string, string>();
        switch (LinkMode)
        {
            case ModeImport:
                if (string.IsNullOrWhiteSpace(secret))
                {
                    SetError(Strings.Get("nostr/link_account/enter_nsec"));
                    return;
                }
                fields["nsec"] = secret!.Trim();
                break;
            case ModeRemote:
                if (!string.IsNullOrWhiteSpace(secret))
                    fields["bunker_url"] = secret!.Trim();
                break;
        }

        try
        {
            await _rpc.BridgesLinkAsync(BridgeId, LinkMode, fields);
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    [RelayCommand]
    public async Task UnlinkAsync()
    {
        SetError(null);
        try
        {
            await _rpc.BridgesUnlinkAsync(BridgeId);
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    // ── Succession-aftermath npub confirm (leg 3) ──
    //
    // The "no / nothing is linked" button is deliberately NOT a separate
    // method here: nostr.md:75 is explicit the remedy is "the existing page
    // machinery" — it reuses UnlinkAsync above verbatim (tui's
    // Action::DismissNpubToNewKey maps directly to Op::Unlink).

    /// <summary>"Yes, that's my npub" — records the confirmation, then
    /// re-reads (non-optimistic, like every other mutation on this page).
    /// </summary>
    public async Task ConfirmNpubAsync()
    {
        SetError(null);
        try
        {
            await _rpc.ConfirmNpubAsync();
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    // ── Content settings ──

    /// <summary>Flip one content flag and re-read. Only the changed key is written — the
    /// provider's wire type is all-optional, so a partial map is the correct shape and a
    /// full read-modify-write would risk clobbering a flag a sibling session changed.
    /// </summary>
    public async Task SetToggleAsync(string key, bool value)
    {
        SetError(null);
        try
        {
            await _rpc.BridgesSetSettingsAsync(
                BridgeId, new[] { BridgeSettingValue.Bool(key, value) });
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    // ── Relays ──

    /// <summary>Validate (shared Rust) then read-modify-write the <c>relay_list</c> JSON
    /// array. A refused URL (malformed, or a private-network literal) surfaces the shared
    /// <c>relay_url_error</c> message and adds no row — the negative assertions
    /// <c>test_relay_invalid_url_rejected</c> / <c>test_relay_private_address_rejected</c>
    /// drive.</summary>
    public async Task AddRelayAsync(string url)
    {
        SetError(null);
        var trimmed = FaunaFfiMethods.TrimmedRelayInput(url ?? string.Empty);
        if (trimmed is null) return;
        var refusal = FaunaFfiMethods.RelayUrlError(trimmed);
        if (refusal is not null)
        {
            SetError(Strings.Resolve(refusal));
            return;
        }
        // ToArray, not ToList: uniffi-bindgen-cs projects a Rust `Vec<String>` as `string[]`
        // (Generated/uniffi/fauna_ffi.cs — `RelayListAppending(string[], string)`).
        var next = FaunaFfiMethods.RelayListAppending(Relays.Select(r => r.Url).ToArray(), trimmed);
        if (next is null) return;

        await WriteRelaysAsync(next);
    }

    public async Task RemoveRelayAsync(int index)
    {
        SetError(null);
        if (index < 0 || index >= Relays.Count) return;
        var next = Relays.Select(r => r.Url).ToList();
        next.RemoveAt(index);
        await WriteRelaysAsync(next);
    }

    private async Task WriteRelaysAsync(IReadOnlyList<string> urls)
    {
        try
        {
            await _rpc.BridgesSetSettingsAsync(
                BridgeId,
                new[] { BridgeSettingValue.Text(RelayListKey, JsonSerializer.Serialize(urls)) })
                ;
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    // ── Follows ──

    public async Task AddFollowAsync(string pubkey, string? petname)
    {
        SetError(null);
        var id = (pubkey ?? string.Empty).Trim();
        if (id.Length == 0) return;
        var name = string.IsNullOrWhiteSpace(petname) ? null : petname!.Trim();
        try
        {
            await _rpc.BridgesAddFollowAsync(BridgeId, id, name);
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    public async Task RemoveFollowAsync(int index)
    {
        SetError(null);
        if (index < 0 || index >= Follows.Count) return;
        var id = Follows[index].Id;
        try
        {
            await _rpc.BridgesRemoveFollowAsync(BridgeId, id);
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    // ── Connect an app (Nostr Connect / NIP-46 bunker invite) ──
    //
    // nostr.md § The nest as the user's NIP-46 signer. The user authorizes each
    // third-party Nostr app with a one-time-revealed, revocable connect string —
    // the mail-credentials interaction shape. The connections themselves are
    // signer rows on the Connected apps page (connected-apps.md — a lift, never a
    // duplication), where they are revoked; this page keeps only the invite start.

    /// <summary>Mint a connect invite and reveal its one-time <c>bunker://…</c>
    /// string, then re-read the bridge row.</summary>
    public async Task ConnectAppAsync()
    {
        SetError(null);
        try
        {
            var invite = await _rpc.NostrBunkerCreateInviteAsync();
            // Published BEFORE the refresh: the reveal is the whole point of the
            // call and must not be lost if the re-read then fails.
            ConnectString = invite.ConnectString;
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

#if PAYMENTS
    // ── Zap signers (the NIP-57 trust root) ──
    //
    // monetization.md § Zap receipts — the trust model; nostr.md § Layout &
    // flow item 7. A kind-9735 zap receipt is signed by the payee's own
    // LNURL/wallet server and is plain signed JSON anyone may mint, so its own
    // signature proves nothing about payment — this roster is what makes one
    // believable: the payee designates which signer pubkey(s) may speak for
    // their money. An empty roster is the meaningful out-of-the-box default
    // (believes nobody), not an unconfigured state.

    /// <summary>Designate a signer. Client-glue 64-hex validation only spares a
    /// guaranteed round trip — the nest refuses a non-64-hex key anyway
    /// (mirrors linux/tui's identical hand check; no shared validator exists for
    /// this shape). Re-lists rather than pushing the typed input locally: the
    /// nest always returns the STORED (lowercase-normalized) row, and only that
    /// form ever matches a real receipt.</summary>
    public async Task AddZapSignerAsync(string pubkey, string? label)
    {
        SetError(null);
        var trimmed = (pubkey ?? string.Empty).Trim();
        if (trimmed.Length == 0) return;
        if (trimmed.Length != 64 || !trimmed.All(Uri.IsHexDigit))
        {
            SetError(Strings.Get("nostr/zap_signers/invalid_pubkey"));
            return;
        }
        try
        {
            await _rpc.NostrZapSignersAddAsync(trimmed, (label ?? string.Empty).Trim());
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Stop trusting a signer, keyed by its STORED pubkey (the row's
    /// own value, not the typed input) — same reason <see cref="RemoveRelayAsync"/>
    /// resolves by value rather than a captured index. Deliberately ungated:
    /// removal is de-escalation, never blocked by the Dim-3 add gate above.</summary>
    public async Task RemoveZapSignerAsync(int index)
    {
        SetError(null);
        if (index < 0 || index >= ZapSigners.Count) return;
        var pubkey = ZapSigners[index].SignerPubkey;
        try
        {
            await _rpc.NostrZapSignersRemoveAsync(pubkey);
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }
#endif
}

/// <summary>One relay row (<c>nostr-relay-item</c>). A record rather than a bare string
/// so the DataTemplate can bind a non-empty <c>AutomationProperties.Name</c>, without
/// which FlaUI counts zero rows (reference_winui_flaui_datatemplate_name).</summary>
public record NostrRelayRow(string Url);

#if PAYMENTS
/// <summary>One Zap-signers roster row (<c>nostr-zap-signer-item</c>), projected
/// for display so the DataTemplate stays a pure <c>x:Bind</c>.
///
/// <para><b>Renders the STORED pubkey, never the typed input</b> — the nest
/// normalizes to lowercase on write and only that form ever matches a real
/// receipt (<see cref="SignerPubkey"/> carries it verbatim for
/// <c>RemoveZapSignerAsync</c>).</para></summary>
public record ZapSignerRow(long Id, string SignerPubkey, string DisplayText)
{
    /// <summary>Mirrors shared Rust <c>fauna_client_nostr::zap_signer_row_text</c>
    /// (not yet UniFFI-exposed to native-consuming clients — apple/android each
    /// carry their own copy of this exact two-branch format): the label, falling
    /// back to "Unnamed signer" when blank, followed by the signer pubkey's
    /// short id.</summary>
    public static ZapSignerRow From(ZapSignerEntry entry)
    {
        var label = string.IsNullOrWhiteSpace(entry.Label)
            ? Strings.Get("nostr/zap_signers/unnamed")
            : entry.Label;
        return new ZapSignerRow(
            entry.Id, entry.SignerPubkey,
            $"{label} — {FaunaFfiMethods.ShortId(entry.SignerPubkey)}");
    }
}
#endif
