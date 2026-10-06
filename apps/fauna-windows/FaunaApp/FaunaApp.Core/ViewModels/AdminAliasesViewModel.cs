using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The admin <c>admin-aliases</c> page (admin.md § 4 Aliases — admin external
/// forwarders, Kind 7). A dumb renderer over the shared <c>ForwarderMachine</c>
/// (<c>libs/fauna-client-mail-settings</c>, exposed by
/// <c>libs/fauna-ffi/src/mail_admin.rs</c> <c>build_forwarders_machine</c>) through
/// its UniFFI <see cref="IForwarderMachine"/> seam — so this VM is unit-testable
/// against a fake (FlaUI flakes on win-arm64, so the VM is the deterministic gate).
/// The machine owns everything per priority #2: the WS-RPC
/// (<c>fauna.bridges.{list,create,delete}_forwarder</c> + <c>list_local_domains</c>),
/// the <c>&lt;pattern&gt;@&lt;local_domain&gt;</c> address render, the hex alias-id
/// round-trip, and the refresh-then-act sequencing. This VM only projects
/// <c>ForwardersSnapshot</c> → bound rows / picker domains / error, and maps the
/// add/delete gestures → <c>ForwarderAction</c>. The catch-all designation (Kind 4)
/// is a deferred slice that rides <c>admin-dns</c>, not this page (admin.md § 4).
/// Mirrors linux <c>build_admin_aliases_page</c> / <c>fetch_forwarders</c> and
/// <see cref="AdminSettingsViewModel"/>'s error/loading conventions.
/// </summary>
public partial class AdminAliasesViewModel : ObservableObject
{
    private readonly IForwarderMachine _machine;

    [ObservableProperty] private bool _isLoading;

    /// <summary>The last action's error (<c>admin-aliases-action-error</c>) — the
    /// snapshot's <c>error</c> after a create/delete, or a hydrate exception.</summary>
    [ObservableProperty] private string? _error;

    /// <summary>The external forwarders, one per <c>admin-aliases-forwarder-list</c> row.</summary>
    public ObservableCollection<ForwarderRowVm> Forwarders { get; } = new();

    /// <summary>Active hosted local domains — the options for the add-form domain
    /// picker (<c>admin-aliases-forwarder-add-domain-select</c>). A forwarder must
    /// live on one of these.</summary>
    public ObservableCollection<string> Domains { get; } = new();

    internal AdminAliasesViewModel(IForwarderMachine machine)
    {
        _machine = machine;
    }

    /// <summary>Initial page load — hydrate the machine (<c>list_forwarders</c> +
    /// <c>list_local_domains</c>) and render its snapshot.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            await _machine.Hydrate();
            RenderSnapshot();
        }
        catch (Exception ex)
        {
            // Hydrate propagates the error without recording it into the snapshot
            // (only dispatch does), so surface the exception directly.
            Error = Strings.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Create an external forwarder (<c>admin-aliases-forwarder-add-submit-button</c>).
    /// A blank domain / pattern / target is a no-op — matching linux's add-form guard
    /// (no hosted domain selected yet, or an empty field). On success the machine
    /// re-reads so the new forwarder appears in <see cref="Forwarders"/>; a nest
    /// rejection (<c>conflicts_with_existing_alias</c> / <c>validate_forward_target</c> /
    /// <c>reserved_local_part</c>) is surfaced to <see cref="Error"/> with the list
    /// left intact.</summary>
    public async Task CreateAsync(string? localDomain, string? pattern, string? target)
    {
        Error = null;
        if (string.IsNullOrWhiteSpace(localDomain)
            || string.IsNullOrWhiteSpace(pattern)
            || string.IsNullOrWhiteSpace(target))
        {
            return;
        }
        await DispatchAndRenderAsync(
            new ForwarderAction.Create(localDomain, pattern.Trim(), target.Trim()));
    }

    /// <summary>Delete a forwarder by its hex alias id
    /// (<c>admin-aliases-forwarder-row-delete-button</c>). The machine re-reads so
    /// the row drops out of the list.</summary>
    public Task DeleteAsync(string aliasIdHex) =>
        DispatchAndRenderAsync(new ForwarderAction.Delete(aliasIdHex));

    /// <summary>Dispatch one action, then render from the resulting snapshot. The
    /// machine clears the prior error, records any failure into the snapshot's
    /// <c>error</c>, and re-reads on success — so the render picks up either the new
    /// list or the error. A thrown <c>DispatchException</c> is only a fallback for
    /// the rare path that leaves no snapshot error.</summary>
    private async Task DispatchAndRenderAsync(ForwarderAction action)
    {
        try
        {
            await _machine.Dispatch(action);
        }
        catch (Exception ex)
        {
            RenderSnapshot();
            if (string.IsNullOrEmpty(Error))
            {
                Error = Strings.Error(ex);
            }
            return;
        }
        RenderSnapshot();
    }

    /// <summary>Project the machine's snapshot onto the bound state: the indexed
    /// forwarder rows, the hosted-domain picker options, and the last action error.</summary>
    private void RenderSnapshot()
    {
        var snap = _machine.Snapshot();
        Forwarders.Clear();
        foreach (var f in snap.forwarders)
        {
            Forwarders.Add(ForwarderRowVm.From(f));
        }
        Domains.Clear();
        foreach (var d in snap.localDomains)
        {
            Domains.Add(d);
        }
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
    }
}

/// <summary>One external-forwarder row (<c>admin-aliases-forwarder-list</c>): the
/// source address (<c>&lt;pattern&gt;@&lt;local_domain&gt;</c>,
/// <c>admin-aliases-forwarder-row-address</c>) + external target
/// (<c>admin-aliases-forwarder-row-target</c>) the row renders, plus the hex alias
/// id the delete button round-trips (<c>admin-aliases-forwarder-row-delete-button</c>
/// → <c>fauna.bridges.delete_forwarder</c>). Projected from the shared
/// <c>ForwarderView</c> (which already builds the address + hex id).</summary>
public sealed record ForwarderRowVm(string AliasIdHex, string Address, string Target)
{
    internal static ForwarderRowVm From(ForwarderView v) =>
        new(v.aliasIdHex, v.address, v.forwardTarget);
}
