using System;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_core;
using uniffi.fauna_onboarding_machine;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The <c>admin-nest</c> page's NAT-mode section (admin.md § N Nest → NAT-mode
/// control) — the post-onboarding change surface for the nest's NAT axis
/// (public/private), the same axis the wizard's <c>nat_mode_choice</c> step
/// confirms once at claim. A dumb projection over the shared
/// <c>fauna_onboarding_machine::AdminNatModeMachine</c>, consumed through its
/// UniFFI-generated <see cref="IAdminNatModeMachine"/> seam (machine-as-seam —
/// no hand-written seam; the page builds the real machine, the unit test fakes
/// the interface; mirrors <c>AdminFilesViewModel</c> /
/// <c>AdminAliasesViewModel</c>'s dispatch-then-reproject shape). Unlike the
/// page's other sections this machine does NOT ride the bearer
/// <see cref="INestRpcClient"/> WS-RPC seam — <c>fauna.setup.nat_mode</c> is a
/// mutable upsert authorized by the payload signature, so the machine takes the
/// raw <c>(nest_url, secret_hex)</c> session pair directly (the page constructs
/// it from <see cref="ISessionAccount"/>, the same creds source as the page's
/// Factory Reset flow). Dispatch-style, no observer: each action awaits then
/// re-reads <c>Snapshot()</c> — the view re-renders after every call.
/// </summary>
internal partial class AdminNatModeViewModel : ObservableObject
{
    private readonly IAdminNatModeMachine _machine;

    /// <summary><c>admin-nest-nat-mode-public-radio</c> checked state.</summary>
    [ObservableProperty] private bool _publicSelected;

    /// <summary><c>admin-nest-nat-mode-private-radio</c> checked state.</summary>
    [ObservableProperty] private bool _privateSelected;

    /// <summary><c>admin-nest-nat-mode-status</c> — the resolved snapshot
    /// message. The idle/saved texts carry the live-vs-restart-applied caveat;
    /// submit/error states render their own <c>admin.nest_page.nat_mode_*</c>
    /// strings (all decided shared-side — this VM never maps state to text
    /// itself).</summary>
    [ObservableProperty] private string _statusText = string.Empty;

    /// <summary><c>admin-nest-nat-mode-save-button</c> enablement, taken
    /// verbatim from the snapshot: the set is mutable, so this stays true after
    /// both a successful save and a failed one (resubmit is always
    /// allowed).</summary>
    [ObservableProperty] private bool _submitEnabled = true;

    internal AdminNatModeViewModel(IAdminNatModeMachine machine)
    {
        _machine = machine;
        Apply(_machine.Snapshot());
    }

    /// <summary>Page show: read <c>fauna.setup.status</c> and pre-select the
    /// current <c>node_mode</c> — the mode can change from another client. A
    /// read failure surfaces as a transient error but leaves save enabled (the
    /// set is safe to submit without a successful read).</summary>
    [RelayCommand]
    private async Task HydrateAsync()
    {
        try
        {
            await _machine.Hydrate();
        }
        catch (Exception)
        {
            // The machine records the read failure into the snapshot's message
            // (admin.nest_page.nat_mode_error_load); the throw carries no
            // separate signal (mirrors AdminAliasesViewModel.LoadAsync).
        }
        finally
        {
            Apply(_machine.Snapshot());
        }
    }

    /// <summary>Radio click (<c>admin-nest-nat-mode-{public,private}-radio</c>).
    /// Recovers from Error/Done back to Choosing; save stays enabled. Exposed
    /// as a plain method (not a <c>[RelayCommand]</c>) because the view calls it
    /// from a re-entrancy-guarded <c>Checked</c> handler on two RadioButtons —
    /// the same shape as <c>OnboardingViewModel.SelectNatMode</c>.</summary>
    public void Select(NodeMode mode)
    {
        _machine.Select(mode);
        Apply(_machine.Snapshot());
    }

    /// <summary>Save: sign + commit the selected mode via the mutable
    /// <c>fauna.setup.nat_mode</c>.</summary>
    [RelayCommand]
    private async Task SubmitAsync()
    {
        try
        {
            await _machine.Submit();
        }
        catch (Exception)
        {
            // As with Hydrate, the cause lands in the snapshot's message
            // (nat_mode_error_transient / _terminal); nothing else to surface.
        }
        finally
        {
            Apply(_machine.Snapshot());
        }
    }

    private void Apply(NatModeSnapshot snap)
    {
        PublicSelected = snap.@selectedMode == NodeMode.Public;
        PrivateSelected = snap.@selectedMode == NodeMode.Private;
        StatusText = Strings.Resolve(snap.@message);
        SubmitEnabled = snap.@submitEnabled;
    }
}
