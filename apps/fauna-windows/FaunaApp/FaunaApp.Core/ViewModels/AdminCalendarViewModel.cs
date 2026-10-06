using System;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The flat admin <c>admin-calendar</c> page (admin.md § 8 Calendar;
/// caldav-server.md § Independent enablement) — the admin's single
/// deployment-wide <b>CalDAV-enable</b> choice. The direct sibling of § 6 Mail's
/// <see cref="AdminMailViewModel"/> mail-enable slice: email and calendar are two
/// independently enableable features of one MDA bridge, so each gets its own admin
/// toggle. A dumb projection over the shared
/// <c>fauna_client_mail_settings::CaldavPolicyMachine</c>, consumed through its
/// UniFFI-generated <see cref="ICaldavPolicyMachine"/> seam (machine-as-seam — no
/// hand-written seam; the page builds the real machine, the unit test fakes the
/// interface). All projection / WS-RPC sequencing lives in shared Rust (priority
/// #2): the <c>get_mail_config</c> hydrate (its <c>caldav_enabled</c> field) +
/// the <c>set_caldav_enabled</c> write + the re-read-after-write; this VM forwards
/// the toggle gesture and re-projects the snapshot. Mirrors AdminMailViewModel's
/// load + dispatch-then-reproject conventions; lifts the flat linux
/// admin-calendar reference.
/// </summary>
public partial class AdminCalendarViewModel : ObservableObject
{
    private readonly ICaldavPolicyMachine _machine;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    /// <summary><c>admin-calendar-enabled-toggle</c> — the deployment-wide
    /// CalDAV-enable flag, reflected from the snapshot after each load / action
    /// (the machine re-reads persisted state after the write, so this reflects the
    /// persisted value, not the optimistic gesture).</summary>
    [ObservableProperty] private bool _caldavEnabled;

    /// <summary><c>admin-calendar-caldav-port-input</c> — the admin-set CalDAV
    /// listener port as an editable string (the <c>admin-mail-*</c> policy-input
    /// pattern), reflected from the snapshot's <c>caldav_port</c> after each load /
    /// save. A string while the admin edits; committed (validated) via
    /// <see cref="SaveCaldavPortAsync"/>.</summary>
    [ObservableProperty] private string _caldavPort = "8443";

    internal AdminCalendarViewModel(ICaldavPolicyMachine machine)
    {
        _machine = machine;
        // Seed the bound state from the machine's pre-hydrate snapshot (the shared
        // CaldavPolicyMachine starts on the catalog default, no I/O), so it is never
        // in an invalid state before the first get_mail_config.
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

    /// <summary>Flip the deployment-wide CalDAV-enable toggle
    /// (<c>admin-calendar-enabled-toggle</c> → <c>set_caldav_enabled</c>; the machine
    /// re-reads persisted state after the write).</summary>
    public Task SetCaldavEnabledAsync(bool enabled)
        => DispatchAsync(new CaldavPolicyAction.SetCaldavEnabled(enabled));

    /// <summary>Validate + commit the admin-set CalDAV port
    /// (<c>admin-calendar-caldav-port-save-button</c> → <c>set_caldav_port</c>).
    /// The port is a u16 in <c>[1, 65535]</c> (admin.md § 8 Calendar; caldav-server.md
    /// § Network exposure); a malformed value surfaces the
    /// <c>caldav_port_invalid</c> message in <c>error-message</c> and does NOT round-trip
    /// the RPC (mirrors the web reference). On a valid value the machine re-reads
    /// persisted state, so <see cref="CaldavPort"/> reflects the stored port.</summary>
    public async Task SaveCaldavPortAsync()
    {
        // Validate locally before dispatching via the shared validator
        // (fauna_core::format::parse_port over the value-format FFI face): a u16 in
        // [1, 65535], trimming whitespace and rejecting 0 / non-numeric / empty /
        // out-of-range. A null return surfaces the page error and skips the RPC.
        if (uniffi.fauna_ffi.FaunaFfiMethods.ParsePort(CaldavPort) is not ushort port)
        {
            Error = Strings.Get("admin/calendar_page/caldav_port_invalid");
            return;
        }
        await DispatchAsync(new CaldavPolicyAction.SetCaldavPort(port));
    }

    /// <summary>Dispatch an action then re-project the snapshot. The machine captures
    /// any user-facing error into <c>snapshot.error</c> (and also throws), so the throw
    /// is swallowed and the error read from the snapshot — matching AdminMailViewModel /
    /// linux; the exception is a fallback only if the snapshot carried no error.</summary>
    private async Task DispatchAsync(CaldavPolicyAction action)
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

    /// <summary>Project the machine snapshot onto the bound state: the CalDAV-enable
    /// flag + the last action error (null when empty).</summary>
    private void Apply(CaldavPolicySnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        CaldavEnabled = snap.caldavEnabled;
        CaldavPort = snap.caldavPort.ToString();
    }
}
