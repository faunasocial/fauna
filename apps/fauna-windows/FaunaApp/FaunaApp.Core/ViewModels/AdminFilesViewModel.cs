using System;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The flat admin <c>admin-files</c> page (admin.md § Files; webdav-server.md
/// § Independent enablement) — the admin's single deployment-wide
/// <b>WebDAV-enable</b> choice. The files sibling of
/// <see cref="AdminContactsViewModel"/>'s CardDAV-enable slice: email + calendar +
/// contacts + files are four independently enableable features of one MDA
/// bridge, so each gets its own admin toggle. A dumb projection over the shared
/// <c>fauna_client_mail_settings::WebdavPolicyMachine</c>, consumed through its
/// UniFFI-generated <see cref="IWebdavPolicyMachine"/> seam (machine-as-seam — no
/// hand-written seam; the page builds the real machine, the unit test fakes the
/// interface). All projection / WS-RPC sequencing lives in shared Rust (priority
/// #2): the <c>get_mail_config</c> hydrate (its <c>webdav_enabled</c> field) +
/// the <c>set_webdav_enabled</c> write + the re-read-after-write; this VM
/// forwards the toggle gesture and re-projects the snapshot. No port property —
/// WebDAV rides the shared DAV listener admin-calendar's port field governs.
/// Mirrors AdminCalendarViewModel / AdminContactsViewModel's load +
/// dispatch-then-reproject conventions.
/// </summary>
public partial class AdminFilesViewModel : ObservableObject
{
    private readonly IWebdavPolicyMachine _machine;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    /// <summary><c>admin-files-webdav-enabled-toggle</c> — the deployment-wide
    /// WebDAV-enable flag, reflected from the snapshot after each load / action
    /// (the machine re-reads persisted state after the write, so this reflects the
    /// persisted value, not the optimistic gesture).</summary>
    [ObservableProperty] private bool _webdavEnabled;

    internal AdminFilesViewModel(IWebdavPolicyMachine machine)
    {
        _machine = machine;
        // Seed the bound state from the machine's pre-hydrate snapshot (the shared
        // WebdavPolicyMachine starts on the catalog default, no I/O), so it is
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

    /// <summary>Flip the deployment-wide WebDAV-enable toggle
    /// (<c>admin-files-webdav-enabled-toggle</c> → <c>set_webdav_enabled</c>;
    /// the machine re-reads persisted state after the write).</summary>
    public Task SetWebdavEnabledAsync(bool enabled)
        => DispatchAsync(new WebdavPolicyAction.SetWebdavEnabled(enabled));

    /// <summary>Dispatch an action then re-project the snapshot. The machine captures
    /// any user-facing error into <c>snapshot.error</c> (and also throws), so the throw
    /// is swallowed and the error read from the snapshot — matching
    /// AdminCalendarViewModel / AdminContactsViewModel; the exception is a fallback
    /// only if the snapshot carried no error.</summary>
    private async Task DispatchAsync(WebdavPolicyAction action)
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

    /// <summary>Project the machine snapshot onto the bound state: the WebDAV-enable
    /// flag + the last action error (null when empty).</summary>
    private void Apply(WebdavPolicySnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        WebdavEnabled = snap.webdavEnabled;
    }
}
