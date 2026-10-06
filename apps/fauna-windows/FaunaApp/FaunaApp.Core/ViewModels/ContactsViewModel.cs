using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Manages contact list and incoming knocks (contact requests). The roster,
/// knock actions, and add-contact knock-send all ride the WS-RPC façade
/// (<c>fauna.{contacts,knocks}.*</c> + <c>fauna.inbox.send</c> via
/// <c>SendKnockAsync</c>). No HTTP plane: the contacts <c>contacts-search-field</c>
/// is a local roster filter (page-side, matching linux), and the standalone Search
/// page rides <c>fauna.search.query</c> — full-text search returns opaque FTS
/// doc-keys, not actor ids, so it cannot drive add-contact. The add-contact panel's
/// own <c>contact-actor-id-lookup</c> resolves a typed <c>64-hex actor-id</c> OR a
/// <c>user@domain</c> handle through the shared federated-discovery seam
/// (<see cref="LookUpRecipientAsync"/>; contacts.md § Where logic lives → Contact
/// lookup by handle).
/// </summary>
public partial class ContactsViewModel : ViewModelBase
{
    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private ContactInfo? _actorIdResult;
    [ObservableProperty] private string? _actorIdError;

    public ObservableCollection<ContactInfo> Contacts { get; } = new();
    public ObservableCollection<KnockInfo> Knocks { get; } = new();

    /// <summary>The owner's open post-succession review roster
    /// (<c>member_reviews_list</c>), CACHED — succession-aftermath.md §
    /// Propagation. Loaded alongside <see cref="Contacts"/> in
    /// <see cref="LoadAsync"/>, BEFORE the contacts collection repopulates,
    /// so every row's <c>contact-unattested-mark</c> paints correctly on
    /// first realize rather than lagging one load behind.</summary>
    internal IReadOnlyList<FfiMemberReview> MemberReviewRoster { get; private set; } = Array.Empty<FfiMemberReview>();

    /// <summary>The <c>contacts-search-field</c> roster filter: narrows the
    /// already-loaded accepted-contacts roster by a case-insensitive substring over
    /// handle + domain + actor-id, through the shared predicate
    /// <c>fauna_core::format::contact_matches_filter</c> (FFI <c>ContactMatchesFilter</c>;
    /// contacts.md § Where logic lives → Contact roster filter). Local-only — never a
    /// nest query. An empty query matches all (the shared fn's contract); the page
    /// short-circuits empty to rebind the live collection instead of a snapshot.
    /// Extracted here (Core) so the row→predicate arg-wiring is unit-tested against the
    /// real FFI dll, rather than buried in the page event handler.</summary>
    public static IReadOnlyList<ContactInfo> FilterRoster(IEnumerable<ContactInfo> contacts, string query) =>
        contacts
            .Where(c => FaunaFfiMethods.ContactMatchesFilter(query, c.Handle, c.Domain, c.ActorId))
            .ToList();

    /// <summary>Which of the three find-user branches the
    /// <c>contact-actor-id-field</c> input takes, from the shared classifier.</summary>
    public enum RecipientKind { ActorId, Handle, Invalid }

    /// <summary>The <c>contact-actor-id-field</c> input classified into the branch
    /// <see cref="LookUpRecipientAsync"/> takes. <see cref="ActorId"/> carries the
    /// lowercased 64-hex for the actor-id kind; <see cref="User"/> + <see cref="Domain"/>
    /// carry the split handle for the handle kind.</summary>
    public readonly record struct RecipientClassification(
        RecipientKind Kind, string ActorId, string User, string Domain);

    /// <summary>Classify a typed find-user input (<c>64-hex actor-id</c> |
    /// <c>user@domain</c> handle | invalid) via the shared
    /// <c>fauna_core::resolve::classify_recipient</c> (FFI
    /// <see cref="FaunaFfiMethods.ClassifyRecipient"/>), the SAME parse linux/android/
    /// web run — never a per-platform length/hex/`@` re-derivation (priority #2/#3/#4;
    /// contacts.md § Where logic lives → Contact lookup by handle). The positional
    /// <c>[kind, actor_id, user, domain]</c> reply is re-keyed into the named branch.
    /// Static so the classify → branch decision is unit-tested against the real FFI dll,
    /// exactly like <see cref="FilterRoster"/>.</summary>
    public static RecipientClassification ClassifyRecipientInput(string? input)
    {
        var p = FaunaFfiMethods.ClassifyRecipient(input?.Trim() ?? string.Empty);
        // p = [kind, actor_id, user, domain]; the slots the kind doesn't carry are "".
        return p[0] switch
        {
            "actor_id" => new(RecipientKind.ActorId, p[1], string.Empty, string.Empty),
            "handle" => new(RecipientKind.Handle, string.Empty, p[2], p[3]),
            _ => new(RecipientKind.Invalid, string.Empty, string.Empty, string.Empty),
        };
    }

    private readonly INestRpcClient _rpc;

    internal ContactsViewModel(INestRpcClient rpc)
    {
        _rpc = rpc;
        // Re-fetch contacts + knocks on every WS reconnect (the linux
        // ResyncRequired sweep set; transport.md § Push events). LoadAsync
        // re-pulls both rosters.
        RefreshOnReconnect(rpc, LoadCommand);
    }

    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            // Loaded BEFORE Contacts repopulates: contact-unattested-mark reads
            // this cache at row-realize time (ContactsPage's ContainerContentChanging),
            // so the roster must already be current when that fires.
            try { MemberReviewRoster = await _rpc.MemberReviewsListAsync(); }
            catch { MemberReviewRoster = Array.Empty<FfiMemberReview>(); }

            var contacts = await _rpc.ContactsListAsync();
            Contacts.Clear();
            foreach (var c in contacts)
                Contacts.Add(c);

            var knocks = await _rpc.KnocksListAsync();
            Knocks.Clear();
            foreach (var k in knocks)
                Knocks.Add(k);
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

    [RelayCommand]
    private async Task AcceptKnockAsync(string actorId)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.KnocksAcceptAsync(actorId);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    [RelayCommand]
    private async Task BlockKnockAsync(string actorId)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.KnocksBlockAsync(actorId);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    [RelayCommand]
    private async Task DismissKnockAsync(string actorId)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.KnocksDismissAsync(actorId);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary><c>contact-confirm</c> — promote an `accepted` roster edge to
    /// `confirmed` (`fauna.contacts.confirm`, contacts.md § User actions).
    /// Reloads on success so the row's status label + this button's own
    /// conditional visibility both re-derive from the fresh roster.</summary>
    [RelayCommand]
    private async Task ConfirmContactAsync(string actorId)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.ContactsConfirmAsync(actorId);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    [RelayCommand]
    private async Task AddFoundContactAsync(string actorId)
    {
        // Send an add-contact knock over fauna.inbox.send (the contacts page's
        // find-by-handle add affordance; the standalone Search page does NOT feed
        // this — its content_ids are opaque FTS keys, not actor ids).
        ErrorMessage = null;
        try
        {
            await _rpc.SendKnockAsync(actorId);
            await LoadAsync();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    private async Task LookUpRecipientAsync(string? input)
    {
        // The federated find-user (contact-actor-id-lookup) path: classify the
        // typed 64-hex actor-id | user@domain handle, then resolve it into a single
        // add-able result. Windows was the LONE client without the handle branch
        // (contacts.md § Where logic lives → Contact lookup by handle); it now mirrors
        // linux/android: classify → (actor_id) build the row directly, (handle)
        // resolve_nest → resolve_handle, (invalid) surface the find error. The add step
        // knocks the RESOLVED actor id (AddFoundContactAsync), never the typed input.
        var c = ClassifyRecipientInput(input);
        ActorIdError = null;
        ActorIdResult = null;
        switch (c.Kind)
        {
            case RecipientKind.Invalid:
                ActorIdError = Strings.Get("contacts/handle_or_actor_id");
                return;

            case RecipientKind.ActorId:
                try
                {
                    // The HTTP `GET /api/v1/contacts/{id}` twin was deleted; resolve from
                    // the WS-RPC roster instead. Not-on-the-roster ⇒ the Pending fallback
                    // (the actor exists but isn't yet a contact), matching the old behaviour.
                    var contacts = await _rpc.ContactsListAsync();
                    ActorIdResult = contacts.FirstOrDefault(
                        x => string.Equals(x.ActorId, c.ActorId, StringComparison.OrdinalIgnoreCase))
                        ?? new ContactInfo(c.ActorId, null, ContactStatus.Pending, null);
                }
                catch
                {
                    ActorIdResult = new ContactInfo(c.ActorId, null, ContactStatus.Pending, null);
                }
                return;

            case RecipientKind.Handle:
                try
                {
                    // Two-hop anonymous federated discovery over the shared FFI seam
                    // (libs/fauna-ffi/src/resolve.rs): home nest SRV-resolves the handle's
                    // domain → owning node URL, then that node maps the handle → actor id.
                    // The home URL is this client's own nest (the same value linux's
                    // FaunaClient::node_url / android's ApiClient.nodeUrl feed resolve_nest).
                    var nodeUrl = await FaunaFfiMethods.ResolveNest(_rpc.HomeUrl, c.Domain);
                    // Pass the typed @domain qualifier so a multi-domain handle echoes it:
                    // on a deployment serving domain1 (primary) + domain2, `bob@domain2`
                    // resolves with domain2 echoed in slot 3 → we display `bob@domain2`
                    // (parity with web/linux/android; mail-multidomain.md § Multi-domain
                    // handles § Resolution). A bare/primary-domain handle still reports the
                    // identity domain. A domain this nest does not serve is rejected
                    // (fauna.actor.domain_not_local) → the catch below surfaces not-found.
                    // resolved = [actor_id, handle, domain]
                    var resolved = await FaunaFfiMethods.ResolveHandle(nodeUrl, c.User, c.Domain);
                    ActorIdResult = new ContactInfo(
                        resolved[0], $"{resolved[1]}@{resolved[2]}", ContactStatus.Pending, null)
                    { Domain = resolved[2] };
                }
                catch
                {
                    ActorIdError = Strings.Get("contacts/handle_not_found");
                }
                return;
        }
    }
}
