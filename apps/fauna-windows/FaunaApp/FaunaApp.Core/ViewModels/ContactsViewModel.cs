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

    /// <summary>The painted roster rows — <see cref="Contacts"/> with the names the
    /// shared overlay face answered (<see cref="ContactOverlayNames"/>). Rebuilt on
    /// every load and whenever <see cref="RefreshNames"/> sees the projection's
    /// revision move. The page binds the list to this, never to
    /// <see cref="Contacts"/>.</summary>
    public ObservableCollection<ContactRow> Rows { get; } = new();

    /// <summary>The painted knock cards (<see cref="Knocks"/> + the sender names).</summary>
    public ObservableCollection<KnockRow> KnockRows { get; } = new();

    private ulong _namesRevision;

    /// <summary>Re-read every painted name from the overlay face. Cheap; the pages
    /// call <see cref="RefreshNamesIfMoved"/> on each manager wake instead.</summary>
    public void RefreshNames()
    {
        using var names = ContactOverlayNames.Open();
        _namesRevision = names.Revision();
        Rows.Clear();
        foreach (var c in Contacts)
            Rows.Add(RowFor(names, c));
        KnockRows.Clear();
        foreach (var k in Knocks)
            KnockRows.Add(new KnockRow(k, names.PeerLabel(null, null, k.ActorId).primary));
    }

    /// <summary>Re-paint the names only when the projection's content moved (the
    /// face's <c>revision()</c>), so a manager wake for a message re-paints nothing.
    /// Returns whether it did.</summary>
    public bool RefreshNamesIfMoved()
    {
        ulong now;
        using (var names = ContactOverlayNames.Open()) now = names.Revision();
        if (now == _namesRevision) return false;
        RefreshNames();
        return true;
    }

    private static ContactRow RowFor(ContactOverlayNames.Reader names, ContactInfo c)
    {
        // Roster row: `peer_label(None, handle, id)` (the face's contract).
        var label = names.PeerLabel(null, c.Handle, c.ActorId);
        return new ContactRow(c, label.primary, label.@public, names.LabelsLine(c.ActorId));
    }

    /// <summary>The <c>contacts-search-field</c> roster filter: narrows the
    /// already-loaded accepted-contacts roster through the shared predicate on the
    /// overlay face (handle, domain, actor id, plus the viewer's nickname and
    /// labels for that person; never notes — contacts.md § The private overlay).
    /// Local-only — never a nest query. An empty query matches all (the shared fn's
    /// contract); the page short-circuits empty to rebind the live collection instead
    /// of a snapshot. Extracted here (Core) so the row→predicate arg-wiring is
    /// unit-tested against the real FFI dll, rather than buried in the page event
    /// handler.</summary>
    public IReadOnlyList<ContactRow> FilterRows(string query)
    {
        using var names = ContactOverlayNames.Open();
        return Rows
            .Where(r => names.MatchesFilter(query, r.Contact.Handle, r.Contact.Domain, r.ActorId))
            .ToList();
    }

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
    /// exactly like <see cref="FilterRows"/>.</summary>
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

    /// <summary>The Find User lookup seam: <c>(homeUrl, handle, domain)</c> →
    /// <c>[actor_id, handle, domain, nest_url]</c>. Defaults to the shared
    /// <see cref="FaunaFfiMethods.FindUser"/>; a unit test swaps it because the lookup is a
    /// live network hop.</summary>
    internal Func<string, string, string?, Task<string[]>> FindUserAsync { get; set; } =
        FaunaFfiMethods.FindUser;

    internal ContactsViewModel(INestRpcClient rpc)
    {
        _rpc = rpc;
        // Re-fetch contacts + knocks on every WS reconnect (the linux
        // ResyncRequired sweep set; transport.md § Push events). LoadAsync
        // re-pulls both rosters.
        RefreshOnReconnect(rpc, LoadCommand);
    }

    /// <summary>Start re-reading the review roster on the shared store-change
    /// notice — call from the page's <c>Page_Loaded</c>, pair with
    /// <see cref="ViewModelBase.CleanupStoreChange"/> on navigate-away.</summary>
    public void WatchStore() => WatchStoreChange(_rpc, StoreResyncAsync);

    /// <summary>The store-change re-read (<c>account-runtime.md</c> § A runtime's
    /// own pump is a source of the notice too, part 4): the succession ledger's
    /// member-review roster paints the <c>contact-unattested-mark</c> badges, so a
    /// person put up for (or cleared from) review shows on the OPEN page with no
    /// re-visit. Answers <c>true</c> only when the roster differs; a failed read
    /// keeps the roster held; skipped while a load is in flight. The contacts and
    /// knocks lists are the nest's, never re-read here.</summary>
    internal async Task<bool> StoreResyncAsync()
    {
        if (IsLoading) return false;
        var roster = await _rpc.MemberReviewsListAsync();
        if (IsLoading || MemberReviewViewModel.SameRoster(roster, MemberReviewRoster)) return false;
        MemberReviewRoster = roster;
        return true;
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
            RefreshNames();
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

    /// <summary>The Find User result's per-peer knock/ask state (family-safety.md §
    /// Child-initiated contact requests → <i>App affordance</i>). Keyed on the resolved
    /// peer, so a new lookup drops the previous peer's refusal instead of carrying it
    /// over (rule (g)); the durable pending list is consulted per peer at render.</summary>
    [ObservableProperty] private KnockAskState _knockAsk = KnockAskState.ForPeer(null);

    /// <summary>What the find-user result shows beside the knock for the peer it
    /// resolved: the durable pending label first, else the ask button after a TYPED
    /// refusal this lookup saw, else nothing. No supervision test (rule (h)).</summary>
    internal ContactAskRender? ContactAskRenderNow() =>
        KnockAsk.Peer is { } peer
            ? WardAskFolds.ContactAskRenderOf(WardAsksCache.ContactAskPending(peer), KnockAsk)
            : null;

    [RelayCommand]
    private async Task AddFoundContactAsync(string actorId)
    {
        // Send an add-contact knock over fauna.inbox.send (the contacts page's
        // find-by-handle add affordance; the standalone Search page does NOT feed
        // this — its content_ids are opaque FTS keys, not actor ids).
        ErrorMessage = null;
        KnockAsk = KnockAsk with { KnockInFlight = true };
        // The peer-nest route is the one the Find User lookup answered, read off the
        // stored result for THIS actor — never re-derived; null for a home-nest actor.
        var route = ActorIdResult is { } found
            && string.Equals(found.ActorId, actorId, StringComparison.OrdinalIgnoreCase)
            && !string.IsNullOrEmpty(found.NestUrl) ? found.NestUrl : null;
        var result = await WardAskFolds.ClassifyKnockAsync(() => _rpc.SendKnockAsync(actorId, route));
        var (next, effect) = WardAskFolds.FoldKnockReply(KnockAsk, actorId, result);
        KnockAsk = next;
        switch (effect)
        {
            case KnockErrorEffect.Clear:
                try { await LoadAsync(); }
                catch (Exception ex) { ShowError(ex); }
                break;
            case KnockErrorEffect.Guardian:
                // Clause (b): the refusal STAYS on error-message — the knock did not
                // happen — with the ask offered beside it.
                SetError(Strings.Get("contacts/guardian_approval_required"));
                break;
            case KnockErrorEffect.Failed failed:
                ShowError(failed.Error);
                break;
        }
    }

    /// <summary><c>contact-request-guardian-button</c> — ask the guardian about the
    /// resolved peer (<c>fauna.family.contact.request</c>). A pending ask is not
    /// re-sent. Mirrors android <c>askGuardian</c> / tui <c>contacts::ask_guardian</c>.</summary>
    [RelayCommand]
    private async Task AskGuardianAsync(string peerActorIdHex)
    {
        var before = KnockAsk;
        if (before.AskInFlight) return;
        if (WardAskFolds.ContactAskRenderOf(WardAsksCache.ContactAskPending(peerActorIdHex), before)
            == ContactAskRender.Pending)
            return;
        KnockAsk = before with { AskInFlight = true };
        try
        {
            await _rpc.FamilyContactRequestAsync(peerActorIdHex);
            KnockAsk = WardAskFolds.FoldContactAsked(KnockAsk, peerActorIdHex);
            ErrorMessage = null;
        }
        catch (Exception ex)
        {
            KnockAsk = WardAskFolds.FoldContactAskFailed(KnockAsk, peerActorIdHex);
            ShowError(ex);
        }
    }

    private void SetFindResult(ContactInfo? result)
    {
        ActorIdResult = result;
        KnockAsk = KnockAskState.ForPeer(result?.ActorId);
    }

    [RelayCommand]
    private async Task LookUpRecipientAsync(string? input)
    {
        // The federated find-user (contact-actor-id-lookup) path: classify the
        // typed 64-hex actor-id | user@domain handle, then resolve it into a single
        // add-able result. Windows was the LONE client without the handle branch
        // (contacts.md § Where logic lives → Contact lookup by handle); it now mirrors
        // linux/android: classify → (actor_id) build the row directly, (handle)
        // find_user, (invalid) surface the find error. The add step knocks the RESOLVED
        // actor id (AddFoundContactAsync), never the typed input.
        var c = ClassifyRecipientInput(input);
        ActorIdError = null;
        SetFindResult(null);
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
                    SetFindResult(contacts.FirstOrDefault(
                        x => string.Equals(x.ActorId, c.ActorId, StringComparison.OrdinalIgnoreCase))
                        ?? new ContactInfo(c.ActorId, null, ContactStatus.Pending, null));
                }
                catch
                {
                    SetFindResult(new ContactInfo(c.ActorId, null, ContactStatus.Pending, null));
                }
                return;

            case RecipientKind.Handle:
                try
                {
                    // The shared find-user lookup (fauna_client_core::find_user, FFI
                    // libs/fauna-ffi/src/resolve.rs): one call answers the handle against
                    // the home nest first and, on a miss, dials the peer nest directly
                    // (federation.md § Client-side peer discovery for a cross-nest knock).
                    // The home URL is this client's own nest (the same value linux's
                    // FaunaClient::node_url / android's ApiClient.nodeUrl feed it).
                    // The typed @domain qualifier is passed so a multi-domain handle echoes it:
                    // on a deployment serving domain1 (primary) + domain2, `bob@domain2`
                    // resolves with domain2 echoed in slot 2 → we display `bob@domain2`
                    // (parity with web/linux/android; mail-multidomain.md § Multi-domain
                    // handles § Resolution). A not-found handle throws → the catch below
                    // surfaces not-found.
                    // found = [actor_id, handle, domain, nest_url]; nest_url is "" for a
                    // home-nest actor and is the peer's URL for a cross-nest one.
                    var found = await FindUserAsync(_rpc.HomeUrl, c.User, c.Domain);
                    SetFindResult(new ContactInfo(
                        found[0], $"{found[1]}@{found[2]}", ContactStatus.Pending, null)
                    {
                        Domain = found[2],
                        NestUrl = string.IsNullOrEmpty(found[3]) ? null : found[3],
                    });
                }
                catch
                {
                    ActorIdError = Strings.Get("contacts/handle_not_found");
                }
                return;
        }
    }
}
