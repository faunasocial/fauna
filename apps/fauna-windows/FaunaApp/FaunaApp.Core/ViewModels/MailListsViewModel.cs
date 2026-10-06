using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// One mailing-list row (<c>mail-lists-list-item</c>), projected from the shared
/// machine's <see cref="ListView"/>. Carries the display strings the DataTemplate binds
/// plus the raw editable fields the Edit sheet pre-populates. Primitive-only public API
/// keeps the row decoupled from the UniFFI-internal <c>ListView</c>.
/// </summary>
public sealed class MailListRow
{
    /// <summary>Lowercase hex list id — the per-row Update/Delete + members-nav key.</summary>
    public required string ListIdHex { get; init; }
    /// <summary>The list's friendly name, alone.</summary>
    public required string FriendlyName { get; init; }
    /// <summary>The full send-from address, rendered beside the friendly name.</summary>
    public required string Address { get; init; }
    /// <summary><c>mail-lists-list-item-name</c> — "{friendly name} — {address}"
    /// (ui.yaml: "List friendly name + send-from address"), matching tui's
    /// <c>format!("{} — {}", friendly_name, address)</c> exactly.</summary>
    public string NameLine => $"{FriendlyName} — {Address}";
    /// <summary><c>mail-lists-list-item-member-count</c> (subscribed only).</summary>
    public required string MemberCount { get; init; }
    /// <summary><c>mail-lists-list-item-last-send</c> (local date; empty = never).</summary>
    public required string LastSend { get; init; }
    /// <summary><c>mail-lists-list-item-quota</c> — sends/recipients today.</summary>
    public required string Quota { get; init; }
    public required string EditLabel { get; init; }
    public required string MembersLabel { get; init; }
    public required string DeleteLabel { get; init; }

    // Raw fields the Edit sheet pre-populates (address is immutable on edit).
    public required string LocalPart { get; init; }
    public required string LocalDomain { get; init; }
    public required string Description { get; init; }
    public required string ListHelpUrl { get; init; }
    public required string ListArchiveUrl { get; init; }
    public uint? RecipientsPerSend { get; init; }

    internal static MailListRow From(ListView v) => new()
    {
        ListIdHex = v.listIdHex,
        FriendlyName = v.friendlyName,
        Address = v.address,
        MemberCount = v.memberCount.ToString(),
        LastSend = v.lastSendAtMs is long ms ? FormatMillisLocal(ms) : string.Empty,
        Quota = $"{v.sendsToday}/{v.recipientsToday}",
        EditLabel = Strings.Get("mail_lists/edit"),
        MembersLabel = Strings.Get("mail_lists/members"),
        DeleteLabel = Strings.Get("mail_lists/delete"),
        LocalPart = v.localPart,
        LocalDomain = v.localDomain,
        Description = v.description,
        ListHelpUrl = v.listHelpUrl,
        ListArchiveUrl = v.listArchiveUrl,
        RecipientsPerSend = v.recipientsPerSend,
    };

    private static string FormatMillisLocal(long ms) =>
        uniffi.fauna_ffi.FaunaFfiMethods.FormatUnixLocalDateMs(ms);
}

/// <summary>
/// The user-facing <c>mail-lists</c> page (docs/goal/behavior/mail-mass-mailing.md
/// § mail-lists page UX) — a person runs their own mailing lists (a list is a sixth
/// alias kind): create / edit / delete a list on one of their owned domains. A dumb
/// projection over the shared <c>fauna_client_mail_settings::MailListsMachine</c>,
/// consumed through its UniFFI-generated <see cref="IMailListsMachine"/> interface
/// (machine-as-seam). All logic lives in shared Rust (priority #2); this VM forwards the
/// actions and re-projects the snapshot after each. Mirrors <see cref="MailAliasesViewModel"/>;
/// lifts apps/fauna-linux/src/settings/mail_lists.rs.
///
/// The client seam went live 2026-07-29 (mail-mass-mailing.md § Implementation status
/// today); every action here reaches the real nest RPCs. The per-row "Members" button
/// opens the mail-list-members settings sub-page scoped to that row's list
/// (<c>MailListsPanel.Members_Click</c>).
/// </summary>
public partial class MailListsViewModel : ObservableObject
{
    private readonly IMailListsMachine _machine;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    /// <summary>Whether the user can create a list: they have at least one owned mail
    /// domain (a list lives on a local_domain — mail-mass-mailing.md). Gates the add button.</summary>
    [ObservableProperty] private bool _canManage;

    /// <summary>The user's owned lists (one <c>mail-lists-list</c> row each).</summary>
    public ObservableCollection<MailListRow> Lists { get; } = new();

    /// <summary>True once a <see cref="LoadAsync"/> round trip has actually completed —
    /// the loading-is-not-empty gate (`ui/README.md` § List pages: loading is not empty;
    /// rule-5 render lift). An empty <see cref="Lists"/> pre-hydrate must NOT read as
    /// "no lists"; only <c>Loaded &amp;&amp; Lists.Count == 0</c> means that. Never set
    /// on a failed load — only a completed round trip flips it.</summary>
    [ObservableProperty] private bool _loaded;

    /// <summary>The user's owned domains (the add-sheet's <c>mail-lists-add-sheet-domain-picker</c>
    /// options), rebuilt from the snapshot on every projection.</summary>
    public ObservableCollection<string> LocalDomains { get; } = new();

    internal MailListsViewModel(IMailListsMachine machine)
    {
        _machine = machine;
    }

    /// <summary>Initial page load: hydrate, then project the snapshot. The transport
    /// already tolerates the post-login connect race for a single RPC (transport.md
    /// § Request lifecycle step 3) — no app-level retry needed here.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            await _machine.Hydrate();
            Apply(_machine.Snapshot());
            Loaded = true;
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

    /// <summary>Create a list with the add-sheet fields (the machine + nest validate the
    /// local-part / domain + enforce cross-user uniqueness).</summary>
    public Task CreateAsync(string friendlyName, string localPart, string localDomain,
        string description, string listHelpUrl, string listArchiveUrl, uint? recipientsPerSend)
        => DispatchAsync(new MailListsAction.Create(new ListDraft(
            friendlyName, localPart, localDomain, description, listHelpUrl, listArchiveUrl, recipientsPerSend)));

    /// <summary>Update an owned list's editable fields (address is immutable on edit, so
    /// the sheet keeps the original local-part / domain).</summary>
    public Task UpdateAsync(string listIdHex, string friendlyName, string localPart, string localDomain,
        string description, string listHelpUrl, string listArchiveUrl, uint? recipientsPerSend)
        => DispatchAsync(new MailListsAction.Update(listIdHex, new ListDraft(
            friendlyName, localPart, localDomain, description, listHelpUrl, listArchiveUrl, recipientsPerSend)));

    /// <summary>Delete an owned list (cascades its members).</summary>
    public Task DeleteAsync(string listIdHex)
        => DispatchAsync(new MailListsAction.Delete(listIdHex));

    private async Task DispatchAsync(MailListsAction action)
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

    private void Apply(MailListsSnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        LocalDomains.Clear();
        foreach (var d in snap.localDomains)
        {
            LocalDomains.Add(d);
        }
        CanManage = LocalDomains.Count > 0;
        Lists.Clear();
        foreach (var l in snap.lists)
        {
            Lists.Add(MailListRow.From(l));
        }
    }
}
