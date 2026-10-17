using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The admin <c>admin-dashboard</c> page — a renderer of the shared card set
/// (<c>admin_dashboard_snapshot()</c>, <c>docs/goal/behavior/admin.md</c> § 1.
/// Dashboard → <i>The card set</i>): the five reads and the one fold live in
/// <c>AdminClient::dashboard_snapshot</c>, so windows resolves each card's label
/// and value and chooses no card of its own. Mirrors tui's lead
/// (<c>apps/fauna-tui/src/admin/dashboard.rs</c>) and android's
/// <c>AdminDashboardVM.kt</c>.
///
/// <para>A failed read has already dropped its own cards from the snapshot; its
/// detail is shown through <c>admin.dashboard.load_error</c> while the other
/// reads' cards still paint. A call that never reaches the nest shows the same
/// way, with no cards.</para>
/// </summary>
public partial class AdminDashboardViewModel : ViewModelBase
{
    private readonly INestRpcClient _rpc;

    // internal, matching every sibling admin VM: INestRpcClient is itself
    // internal (the test + WinUI assemblies see it via [InternalsVisibleTo]).
    internal AdminDashboardViewModel(INestRpcClient rpc) { _rpc = rpc; }

    [ObservableProperty] private bool _isLoading;

    /// <summary>True once a snapshot has answered — the cards it carries (none,
    /// when every read failed) are the nest's, not a pre-load placeholder.</summary>
    [ObservableProperty] private bool _hasLoaded;

    /// <summary>One entry per <c>admin-stat-card</c>, in the fold's order.</summary>
    public ObservableCollection<AdminStatCardVm> Cards { get; } = new();

    internal async Task LoadAsync()
    {
        IsLoading = true;
        try
        {
            var snapshot = await _rpc.AdminDashboardSnapshotAsync();
            Cards.Clear();
            foreach (var card in snapshot.@stats)
                Cards.Add(new AdminStatCardVm(Strings.Resolve(card.@label), Strings.Resolve(card.@value)));
            HasLoaded = true;
            SetError(snapshot.@error is { } detail ? LoadError(detail) : null);
        }
        catch (Exception ex)
        {
            // No read answered: an earlier load's cards are no longer the nest's.
            Cards.Clear();
            SetError(LoadError(Strings.Error(ex)));
        }
        finally
        {
            IsLoading = false;
        }
    }

    private static string LoadError(string detail) =>
        Strings.Format("admin/dashboard/load_error", detail);
}

/// <summary>One <c>admin-stat-card</c> — its label and value already resolved,
/// so the page stays a pure renderer.</summary>
public sealed record AdminStatCardVm(string Label, string Value);
