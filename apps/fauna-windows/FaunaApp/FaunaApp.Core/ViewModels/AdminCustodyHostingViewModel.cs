using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The admin <c>admin-custody-hosting</c> page — the nest-wide custody-hosting
/// registry (<c>docs/goal/architecture/account-data-plane.md</c> § Two-sided
/// bounds). windows is the
/// last of 7 apps owing this leg. Read is <c>fauna.admin.custody_hosting.list</c>
/// (<see cref="INestRpcClient.AdminCustodyHostingListAsync"/>, already folded by
/// the shared <c>admin_hosting_rows</c> projection at the FFI boundary — rows
/// arrive pre-ordered heaviest-hold-first, never re-sorted here); write is
/// <c>fauna.admin.custody_hosting.remove</c>, behind an inline arm/confirm
/// because removing frees bytes that stop deliberately does not. Mirrors the
/// linux reference (<c>apps/fauna-linux/src/client.rs</c>'s
/// <c>fetch_custody_hosting</c>/<c>remove_custody_hosting</c>), tui's lead
/// (<c>apps/fauna-tui/src/admin/custody_hosting.rs</c>), and apple's
/// <c>AdminCustodyHostingVM.swift</c>.
///
/// <para><b><see cref="HasLoaded"/> is the pre-hydrate flag</b> — "nobody has
/// asked this nest to hold anything" and "the read has not answered yet" are
/// different facts, and the page must never render the reassuring one for the
/// unknown one (mirrors <c>rows == nil</c> on apple/android).</para>
/// </summary>
public partial class AdminCustodyHostingViewModel : ViewModelBase
{
    private readonly INestRpcClient _rpc;

    // internal, matching every sibling admin VM: INestRpcClient is itself
    // internal (the test + WinUI assemblies see it via [InternalsVisibleTo]).
    internal AdminCustodyHostingViewModel(INestRpcClient rpc) { _rpc = rpc; }

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private bool _isBusy;
    [ObservableProperty] private bool _hasLoaded;

    /// <summary>The remove's own verdict (Removed / Removed-and-store-freed /
    /// already-gone) — chrome text, not an error (<c>removed == false</c> is an
    /// honest no-op). No ui.yaml id of its own, mirroring tui's
    /// <c>Element::chrome(status)</c> / apple's <c>status</c>.</summary>
    [ObservableProperty] private string? _status;

    public ObservableCollection<AdminHostingRowVm> Rows { get; } = new();

    internal async Task LoadAsync()
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

    private async Task RefreshAsync()
    {
        var rows = await _rpc.AdminCustodyHostingListAsync();
        Rows.Clear();
        foreach (var row in rows) Rows.Add(AdminHostingRowVm.From(row));
        HasLoaded = true;
    }

    /// <summary>Drop one row, keyed by the <c>(host, grant)</c> pair a
    /// <see cref="Rows"/> entry carries (never a painted index — a re-read can
    /// reorder rows), then re-fetch.</summary>
    internal async Task RemoveAsync(string hostActorId, byte[] grantId)
    {
        SetError(null);
        Status = null;
        IsBusy = true;
        try
        {
            var reply = await _rpc.AdminCustodyHostingRemoveAsync(hostActorId, grantId);
            Status = !reply.@removed ? Strings.Get("admin/custody_hosting/remove_missing")
                : reply.@storeDropped ? Strings.Get("admin/custody_hosting/removed_with_store")
                : Strings.Get("admin/custody_hosting/removed");
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            IsBusy = false;
        }
    }
}

/// <summary>One <c>admin-custody-hosting-row</c> — every value already fully
/// composed (byte sizes / the *Default* budget honesty property / the receipt
/// word), so the page stays a pure renderer with no formatting logic of its
/// own. <see cref="HostActorId"/> + <see cref="GrantId"/> are the raw
/// <c>(host, grant)</c> pair the remove door is keyed by; <see cref="Key"/> is
/// their string form for the page-local armed-confirm slot.</summary>
public sealed record AdminHostingRowVm(
    string HostActorId,
    byte[] GrantId,
    string Key,
    string HostText,
    string OwnerText,
    string UrlText,
    string BudgetText,
    string HeldText,
    string StoppedText,
    string ReceiptText)
{
    internal static AdminHostingRowVm From(FfiAdminHostingRow row)
    {
        var key = $"{row.@hostActorId}:{Convert.ToHexString(row.@grantId).ToLowerInvariant()}";
        return new AdminHostingRowVm(
            HostActorId: row.@hostActorId,
            GrantId: row.@grantId,
            Key: key,
            HostText: FaunaFfiMethods.ShortId(row.@hostActorId),
            OwnerText: FaunaFfiMethods.ShortId(row.@ownerActorId),
            UrlText: row.@ownerNestUrl,
            // `0` means the row carries no cap and the pump substitutes the
            // hard-coded default — the leg must render *Default*, never `0 B`
            // (a printed `0 B` would state the opposite of the truth).
            BudgetText: row.@retainedBytesCap == 0
                ? Strings.Get("admin/custody_hosting/budget_default")
                : ValueFormat.ByteSize((ulong)row.@retainedBytesCap),
            HeldText: ValueFormat.ByteSize((ulong)row.@heldBytes),
            // A stopped row still holds its bytes — remove exists precisely
            // because stop alone does not free them.
            StoppedText: row.@stopped
                ? Strings.Get("admin/custody_hosting/stopped")
                : Strings.Get("admin/custody_hosting/active"),
            ReceiptText: ReceiptTextFor(row.@receiptState));
    }

    private static string ReceiptTextFor(FfiReceiptState state) => state switch
    {
        FfiReceiptState.Fresh => Strings.Get("admin/custody_hosting/receipt_fresh"),
        FfiReceiptState.Stale => Strings.Get("admin/custody_hosting/receipt_stale"),
        FfiReceiptState.NoReceiptYet => Strings.Get("admin/custody_hosting/receipt_none"),
        _ => throw new ArgumentOutOfRangeException(nameof(state)),
    };
}
