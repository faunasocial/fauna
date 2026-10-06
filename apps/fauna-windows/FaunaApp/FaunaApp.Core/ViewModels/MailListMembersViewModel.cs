using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// One member row (<c>mail-list-members-list-item</c>), projected from the shared
/// machine's <see cref="MemberView"/>. <see cref="IsSubscribed"/> gates the per-row
/// unsubscribe/resubscribe buttons (sticky-unsubscribe contract). Primitive-only public
/// API keeps the row decoupled from the UniFFI-internal <c>MemberView</c> / <c>MemberStatus</c>.
/// </summary>
public sealed class MailListMemberRow
{
    /// <summary><c>mail-list-members-list-item-address</c> — the per-row action key.</summary>
    public required string Address { get; init; }
    /// <summary><c>mail-list-members-list-item-subscribed-at</c> (local date; empty = unset).</summary>
    public required string SubscribedAt { get; init; }
    /// <summary><c>mail-list-members-list-item-status</c> (Subscribed / Unsubscribed).</summary>
    public required string StatusBadge { get; init; }
    /// <summary>Unsubscribe is active when subscribed; resubscribe when not.</summary>
    public required bool IsSubscribed { get; init; }
    /// <summary>Resubscribe-button <c>IsEnabled</c> — the inverse of <see cref="IsSubscribed"/>
    /// as a bindable property (x:Bind can't negate inline; win.md § x:Bind no nested calls).</summary>
    public bool CanResubscribe => !IsSubscribed;
    public required string UnsubscribeLabel { get; init; }
    public required string ResubscribeLabel { get; init; }

    internal static MailListMemberRow From(MemberView v) => new()
    {
        Address = v.address,
        SubscribedAt = v.subscribedAtMs is long ms ? FormatMillisLocal(ms) : string.Empty,
        // Status label comes from the canonical shared map
        // (fauna_client_mail_settings::member_status_label) so all 7 apps render
        // the identical MemberStatus → i18n-key mapping; windows resolves the
        // returned LocalizedText through its own i18n runtime (priority #1/#2).
        StatusBadge = Strings.Resolve(FaunaClientMailSettingsMethods.MemberStatusLabel(v.status)),
        IsSubscribed = v.status == MemberStatus.Subscribed,
        UnsubscribeLabel = Strings.Get("mail_lists/unsubscribe"),
        ResubscribeLabel = Strings.Get("mail_lists/resubscribe"),
    };

    private static string FormatMillisLocal(long ms) =>
        uniffi.fauna_ffi.FaunaFfiMethods.FormatUnixLocalDateMs(ms);
}

/// <summary>
/// The user-facing <c>mail-list-members</c> page (docs/goal/behavior/mail-mass-mailing.md
/// § mail-list-members page) — managing the members of one mailing list: the
/// subscribed/unsubscribed summary, add a member, batch-import addresses, and per-member
/// unsubscribe / resubscribe. A dumb projection over the shared
/// <c>fauna_client_mail_settings::MailListMembersMachine</c> (scoped to one list_id),
/// consumed through its UniFFI-generated <see cref="IMailListMembersMachine"/> interface.
/// All logic lives in shared Rust (priority #2). Lifts
/// apps/fauna-linux/src/settings/mail_list_members.rs.
///
/// In the full app this page is opened scoped to a specific list (the mail-lists row's
/// Members button); in the embedded settings seed (no inter-page routing) the panel
/// constructs it with a placeholder list id for ID-conformance, like the linux lead.
/// </summary>
public partial class MailListMembersViewModel : ObservableObject
{
    private readonly IMailListMembersMachine _machine;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;
    /// <summary><c>mail-list-members-summary</c> — "N subscribed · M unsubscribed".</summary>
    [ObservableProperty] private string _summary = string.Empty;

    /// <summary>The list's members (one <c>mail-list-members-list</c> row each).</summary>
    public ObservableCollection<MailListMemberRow> Members { get; } = new();

    /// <summary>True once a <see cref="LoadAsync"/> round trip has actually completed —
    /// the loading-is-not-empty gate (`ui/README.md` § List pages: loading is not empty;
    /// rule-5 render lift). An empty <see cref="Members"/> pre-hydrate must NOT read as
    /// "no members"; only <c>Loaded &amp;&amp; Members.Count == 0</c> means that. Never set
    /// on a failed load — only a completed round trip flips it.</summary>
    [ObservableProperty] private bool _loaded;

    internal MailListMembersViewModel(IMailListMembersMachine machine)
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

    /// <summary>Add a single member by address.</summary>
    public Task AddMemberAsync(string address)
        => DispatchAsync(new MailListMembersAction.AddMember(address));

    /// <summary>Batch-import addresses (one per line; the machine parses + tallies).</summary>
    public Task BatchImportAsync(string addresses)
        => DispatchAsync(new MailListMembersAction.BatchImport(addresses));

    /// <summary>Unsubscribe one member (manual; sets unsubscribed_at).</summary>
    public Task UnsubscribeAsync(string address)
        => DispatchAsync(new MailListMembersAction.Unsubscribe(address));

    /// <summary>Resubscribe one member.</summary>
    public Task ResubscribeAsync(string address)
        => DispatchAsync(new MailListMembersAction.Resubscribe(address));

    private async Task DispatchAsync(MailListMembersAction action)
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

    private void Apply(MailListMembersSnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        // windows resw is flat → substitute the named placeholders in C#
        // (reference_i18n_placeholder_named_not_numeric).
        Summary = Strings.Get("mail_lists/summary_fmt")
            .Replace("{subscribed}", snap.subscribedCount.ToString())
            .Replace("{unsubscribed}", snap.unsubscribedCount.ToString());
        Members.Clear();
        foreach (var m in snap.members)
        {
            Members.Add(MailListMemberRow.From(m));
        }
    }
}
