using System;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The flat admin <c>admin-contacts</c> page (admin.md § Contacts;
/// carddav-server.md § Independent enablement) — the admin's single
/// deployment-wide <b>CardDAV-enable</b> choice. The contacts sibling of
/// <see cref="AdminCalendarViewModel"/>'s CalDAV-enable slice: email + calendar +
/// contacts are three independently enableable features of one MDA bridge, so
/// each gets its own admin toggle. A dumb projection over the shared
/// <c>fauna_client_mail_settings::CarddavPolicyMachine</c>, consumed through its
/// UniFFI-generated <see cref="ICarddavPolicyMachine"/> seam (machine-as-seam — no
/// hand-written seam; the page builds the real machine, the unit test fakes the
/// interface). All projection / WS-RPC sequencing lives in shared Rust (priority
/// #2): the <c>get_mail_config</c> hydrate (its <c>carddav_enabled</c> field) +
/// the <c>set_carddav_enabled</c> write + the re-read-after-write; this VM
/// forwards the toggle gesture and re-projects the snapshot. No port property —
/// CardDAV rides the shared DAV listener admin-calendar's port field governs.
/// Mirrors AdminCalendarViewModel's load + dispatch-then-reproject
/// conventions.
/// </summary>
public partial class AdminContactsViewModel : ObservableObject
{
    private readonly ICarddavPolicyMachine _machine;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    /// <summary><c>admin-contacts-carddav-enabled-toggle</c> — the deployment-wide
    /// CardDAV-enable flag, reflected from the snapshot after each load / action
    /// (the machine re-reads persisted state after the write, so this reflects the
    /// persisted value, not the optimistic gesture).</summary>
    [ObservableProperty] private bool _carddavEnabled;

    internal AdminContactsViewModel(ICarddavPolicyMachine machine)
    {
        _machine = machine;
        // Seed the bound state from the machine's pre-hydrate snapshot (the shared
        // CarddavPolicyMachine starts on the catalog default, no I/O), so it is
        // never in an invalid state before the first get_mail_config.
        Apply(_machine.Snapshot());
    }

    /// <summary>Initial page load: hydrate from <c>get_mail_config</c>, then project the
    /// snapshot. The transport already tolerates the post-login connect race for a single
    /// RPC (transport.md § Request lifecycle step 3) — no app-level retry needed here.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            await _machine.Hydrate();
            Apply(_machine.Snapshot());
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Flip the deployment-wide CardDAV-enable toggle
    /// (<c>admin-contacts-carddav-enabled-toggle</c> → <c>set_carddav_enabled</c>;
    /// the machine re-reads persisted state after the write).</summary>
    public Task SetCarddavEnabledAsync(bool enabled)
        => DispatchAsync(new CarddavPolicyAction.SetCarddavEnabled(enabled));

    /// <summary>Dispatch an action then re-project the snapshot. The machine captures
    /// any user-facing error into <c>snapshot.error</c> (and also throws), so the throw
    /// is swallowed and the error read from the snapshot — matching AdminCalendarViewModel;
    /// the exception is a fallback only if the snapshot carried no error.</summary>
    private async Task DispatchAsync(CarddavPolicyAction action)
    {
        try
        {
            await _machine.Dispatch(action);
            Apply(_machine.Snapshot());
        }
        catch (Exception ex)
        {
            Apply(_machine.Snapshot());
            if (string.IsNullOrEmpty(Error)) Error = Strings.Error(ex);
        }
    }

    /// <summary>Project the machine snapshot onto the bound state: the CardDAV-enable
    /// flag + the last action error (null when empty).</summary>
    private void Apply(CarddavPolicySnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        CarddavEnabled = snap.carddavEnabled;
    }
}
