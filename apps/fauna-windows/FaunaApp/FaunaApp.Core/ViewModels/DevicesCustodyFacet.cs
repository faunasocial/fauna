using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_client_capabilities;
using uniffi.fauna_conversations;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Settings → Devices page's T16 custody facet (ui/devices.md § Custody
/// facet): piece 1's keyless-posture marker, piece 2's custodian rows, piece 3's
/// held-for-others rows and offer consent cards, and the <c>custody-mint-*</c>
/// offer flow. Every act runs through the shared
/// <c>fauna_client_custody::run_custody_act</c> behind the UniFFI face
/// (<see cref="INestRpcClient"/>'s <c>Custody*Async</c> seam); nothing here
/// derives a row, a label or a posture — the page paints what this holds.
///
/// <para>The windows counterpart of linux's <c>CustodyView</c> +
/// <c>run_custody_gesture</c> (<c>apps/fauna-linux/src/views/devices_folders/</c>)
/// and tui's <c>settings/devices.rs</c>: one dispatcher per gesture, each
/// repainting from the re-folded facet and answering on <see cref="Error"/>,
/// which the page folds into its <c>error-message</c> precedence so a machine
/// tick cannot wipe it (e2e convention 11: never a silent drop). The WinUI page
/// is not reachable from the unit-test assembly, so the rules live here, where
/// <c>DevicesCustodyFacetTests</c> pins them.</para>
///
/// <para>⚠ No <c>ConfigureAwait(false)</c>: the page awaits these and repaints
/// on the continuation (reference_windows_vm_configureawait_comexception).</para>
/// </summary>
internal sealed class DevicesCustodyFacet
{
    private readonly HashSet<string> _offerTargets = new();
    private Dictionary<string, bool> _keyless = new();

    /// <summary>The last successfully-folded facet. A <c>null</c> fold (the
    /// config unreadable this pass) keeps it — a live list never blanks on a
    /// transient.</summary>
    public CustodyFacetView? Facet { get; private set; }

    /// <summary>The last custody gesture's error, or <c>null</c>. Cleared by the
    /// next gesture that succeeds; never by a load.</summary>
    public string? Error { get; private set; }

    /// <summary>The open mint flow's host options, or <c>null</c> while the flow
    /// is closed. Never empty — an empty candidate list answers on
    /// <see cref="Error"/> instead of opening a picker whose confirm can never
    /// succeed.</summary>
    public IReadOnlyList<CustodyMintCandidateView>? MintCandidates { get; private set; }

    /// <summary>Whether the consent card for <paramref name="grantId"/> renders
    /// <c>custody-offer-target-select</c> — the shared
    /// <c>custody_offer_shows_target_select</c> answer, read on every fold.</summary>
    public bool ShowsTargetSelect(byte[] grantId) => _offerTargets.Contains(Hex(grantId));

    /// <summary>Whether the roster row granted to <paramref name="principal"/>
    /// wears <c>device-keyless-posture-badge</c>. A row the last read did not
    /// cover — or one with no principal — is unmarked: the marker never rests
    /// on an unknown.</summary>
    public bool IsKeyless(string? principal)
        => principal is not null && _keyless.TryGetValue(principal, out var keyless) && keyless;

    /// <summary>The page's load edge: one ceremony drive pass (fire-and-forget,
    /// skipped without a session — its own try, since a drive miss only delays
    /// freshness), then the fold. Never throws.</summary>
    public async Task LoadAsync(INestRpcClient rpc, ConversationsSession? session)
    {
        if (session is not null)
        {
            try
            {
                await rpc.CustodyDriveAsync(session);
            }
            catch (Exception ex)
            {
                ShellLog.Warn("DevicesCustodyFacet", $"[custody] drive failed: {ex.GetType().Name}: {ex.Message}");
            }
        }
        try
        {
            if (await rpc.CustodyFacetLoadAsync() is { } facet) await AdoptAsync(rpc, facet);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("DevicesCustodyFacet", $"[custody] load failed: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>Re-read piece 1's posture for the roster's principals, in order
    /// (the shared <c>keyless_posture</c> join). A failed read keeps the last
    /// answer. Never throws.</summary>
    public async Task LoadKeylessPostureAsync(INestRpcClient rpc, IReadOnlyList<string?> principals)
    {
        try
        {
            var marks = await rpc.DevicesKeylessPostureAsync(principals);
            var next = new Dictionary<string, bool>();
            for (var i = 0; i < principals.Count && i < marks.Count; i++)
            {
                if (principals[i] is { } p) next[p] = marks[i];
            }
            _keyless = next;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("DevicesCustodyFacet", $"[keyless-posture] read failed: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary><c>custody-holder-revoke-button</c>. Keyed by the row's grant id
    /// and accept-bound key, never a row index a refold can re-point.</summary>
    public Task RevokeAsync(INestRpcClient rpc, byte[] grantId, byte[]? holder)
        => RunAsync(rpc, () => rpc.CustodyRevokeAsync(grantId, holder),
            error => Strings.Format("devices/error_revoke_custody", error));

    /// <summary><c>custody-offer-accept-button</c>. <paramref name="onNest"/> is
    /// the target select's answer (false where the select is absent). The accept
    /// is posted by the drive over the session, so none → say so.</summary>
    public Task AcceptAsync(INestRpcClient rpc, ConversationsSession? session, byte[] grantId, bool onNest)
    {
        if (session is null)
        {
            Error = Strings.Get("devices/custody_mint_no_contacts");
            return Task.CompletedTask;
        }
        return RunAsync(rpc, () => rpc.CustodyAcceptAsync(session, grantId, onNest));
    }

    /// <summary><c>custody-offer-decline-button</c>.</summary>
    public Task DeclineAsync(INestRpcClient rpc, byte[] grantId)
        => RunAsync(rpc, () => rpc.CustodyDeclineAsync(grantId));

    /// <summary><c>custody-held-budget-input</c>'s commit. The typed text is
    /// parsed by the shared <c>parse_byte_size</c>; an unparseable (or zero)
    /// budget makes no call and says so — tui's and linux's answer.</summary>
    public Task SetBudgetAsync(INestRpcClient rpc, byte[] grantId, string typed)
    {
        if (FaunaFfiMethods.ParseByteSize(typed) is not { } cap || cap == 0)
        {
            Error = Strings.Get("backups/backup_destination_capacity_invalid");
            return Task.CompletedTask;
        }
        return RunAsync(rpc, () => rpc.CustodySetBudgetAsync(grantId, cap));
    }

    /// <summary><c>custody-held-stop-button</c> — pauses the hold, keeps the bytes.</summary>
    public Task StopAsync(INestRpcClient rpc, byte[] grantId)
        => RunAsync(rpc, () => rpc.CustodyStopAsync(grantId));

    /// <summary><c>custody-held-remove-button</c> — the reclaim.</summary>
    public Task RemoveAsync(INestRpcClient rpc, byte[] grantId)
        => RunAsync(rpc, () => rpc.CustodyRemoveAsync(grantId));

    /// <summary><c>custody-mint-button</c>: read the host options (this
    /// account's 1:1 conversations — the request travels over one) and open the
    /// flow over them, or answer <c>devices.custody_mint_no_contacts</c> when
    /// there is no one to ask.</summary>
    public void OpenMint(INestRpcClient rpc, ConversationsSession? session)
    {
        IReadOnlyList<CustodyMintCandidateView> candidates = Array.Empty<CustodyMintCandidateView>();
        if (session is not null)
        {
            try
            {
                candidates = rpc.CustodyMintCandidates(session);
            }
            catch (Exception ex)
            {
                Error = Strings.Error(ex);
                return;
            }
        }
        if (candidates.Count == 0)
        {
            MintCandidates = null;
            Error = Strings.Get("devices/custody_mint_no_contacts");
            return;
        }
        Error = null;
        MintCandidates = candidates;
    }

    /// <summary><c>custody-mint-cancel-button</c>.</summary>
    public void CloseMint() => MintCandidates = null;

    /// <summary><c>custody-mint-confirm-button</c> over the chosen candidate,
    /// passed back unchanged (the page picks a row, it never assembles a
    /// channel). The flow closes once the request is sent.</summary>
    public async Task MintAsync(INestRpcClient rpc, ConversationsSession? session, CustodyMintCandidateView chosen)
    {
        if (session is null)
        {
            Error = Strings.Get("devices/custody_mint_no_contacts");
            return;
        }
        if (await RunAsync(rpc, () => rpc.CustodyMintAsync(session, chosen.@host, chosen.@channelHex)))
            MintCandidates = null;
    }

    /// <summary>Run one act: repaint from its re-folded facet (a <c>null</c>
    /// facet keeps the painted rows) and put its error — or the thrown one — on
    /// <see cref="Error"/>. Returns whether the act succeeded.</summary>
    private async Task<bool> RunAsync(
        INestRpcClient rpc, Func<Task<FfiCustodyActOutcome>> act, Func<string, string>? format = null)
    {
        try
        {
            var outcome = await act();
            if (outcome.@facet is { } facet) await AdoptAsync(rpc, facet);
            Error = outcome.@error is { } e ? (format?.Invoke(e) ?? e) : null;
            return outcome.@error is null;
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
            return false;
        }
    }

    private async Task AdoptAsync(INestRpcClient rpc, CustodyFacetView facet)
    {
        Facet = facet;
        _offerTargets.Clear();
        foreach (var offer in facet.@offers)
        {
            try
            {
                if (await rpc.CustodyOfferShowsTargetSelectAsync(offer)) _offerTargets.Add(Hex(offer.@grantId));
            }
            catch (Exception ex)
            {
                // No select is the safe answer: the accept then binds this device.
                ShellLog.Warn("DevicesCustodyFacet", $"[custody] target-select read failed: {ex.GetType().Name}: {ex.Message}");
            }
        }
    }

    private static string Hex(byte[] bytes) => Convert.ToHexString(bytes);
}
