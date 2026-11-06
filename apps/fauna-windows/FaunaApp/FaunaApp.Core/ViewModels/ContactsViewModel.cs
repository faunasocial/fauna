using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_archive_import_machine;
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

    /// <summary>The painted roster: <see cref="Rows"/> grouped by relationship status in
    /// the shared order (<see cref="GroupByStatus"/>; contacts.md § Where logic lives →
    /// Roster status grouping), rebuilt with <see cref="Rows"/>. The page binds its
    /// grouped list to this; <see cref="Rows"/> stays the roster's own order, the one
    /// <see cref="FilterRows"/> narrows.</summary>
    public ObservableCollection<ContactRosterSection> Sections { get; } = new();

    /// <summary>The painted knock cards (<see cref="Knocks"/> + the sender names).</summary>
    public ObservableCollection<KnockRow> KnockRows { get; } = new();

    private ulong _namesRevision;

    /// <summary>The account's archive links (phase two — <c>archive-import.md</c> §
    /// Linking and merging, ruling 8): the views each row's
    /// <see cref="ContactRow.ArchiveLinks"/> paints from, re-read with the roster.</summary>
    private readonly ArchiveLinksHost _links;

    /// <summary>The <see cref="ArchiveLinksHost.Revision"/> the rows were last painted at.</summary>
    private ulong _linksRevision;

    /// <summary>Re-read every painted name from the overlay face. Cheap; the pages
    /// call <see cref="RefreshNamesIfMoved"/> on each manager wake instead. Each row's
    /// archive-link paints are rebuilt from the host's current views in the same pass.</summary>
    public void RefreshNames()
    {
        using var names = ContactOverlayNames.Open();
        _namesRevision = names.Revision();
        _linksRevision = _links.Revision;
        var views = _links.Views;
        Rows.Clear();
        foreach (var c in Contacts)
            Rows.Add(RowFor(names, c, views));
        Sections.Clear();
        foreach (var section in GroupByStatus(Rows))
            Sections.Add(section);
        PaintKnockRows(names);
    }

    /// <summary>The roster grouped by relationship status (contacts.md § Where logic
    /// lives → Roster status grouping): one <see cref="ContactRosterSection"/> per status
    /// a row holds, in the shared <c>contact_roster_group_order</c> (FFI
    /// <see cref="FaunaFfiMethods.ContactRosterGroupOrder"/> — confirmed, accepted,
    /// pending, blocked, then any status this build does not name, first-seen), each
    /// holding its rows in their given order. A status no row holds makes no section,
    /// so a section the filter empties paints nothing. Keyed on the wire string
    /// (<see cref="ContactInfo.WireStatus"/>), never the enum, so an unnamed status
    /// keeps a section of its own.</summary>
    public static IReadOnlyList<ContactRosterSection> GroupByStatus(IReadOnlyList<ContactRow> rows)
    {
        var order = FaunaFfiMethods.ContactRosterGroupOrder(
            rows.Select(r => r.Contact.WireStatus).ToArray());
        return order
            .Select(status => new ContactRosterSection(
                status, rows.Where(r => r.Contact.WireStatus == status)))
            .ToList();
    }

    /// <summary>The grouped roster narrowed by the <c>contacts-search-field</c> query:
    /// <see cref="FilterRows"/>, then <see cref="GroupByStatus"/> — the filter narrows
    /// within sections and an emptied section drops out.</summary>
    public IReadOnlyList<ContactRosterSection> FilterSections(string query) =>
        GroupByStatus(FilterRows(query));

    /// <summary>Re-paint the rows when the archive-link views moved (the page calls this
    /// on <see cref="ArchiveLinksHost.Changed"/>: a question arrived, an answer or unlink
    /// landed). Returns whether it did.</summary>
    public bool RepaintArchiveLinksIfMoved()
    {
        if (_links.Revision == _linksRevision) return false;
        RefreshNames();
        return true;
    }

    /// <summary><c>contact-archive-link-yes</c> / <c>contact-archive-link-no</c> — the
    /// row's same-person answer, once, either way, through the shared
    /// <c>answer_link_token</c> with the row's own contact and platform.</summary>
    public async Task AnswerArchiveLinkAsync(ArchiveLinkRowPaint paint, bool yes)
    {
        await _links.AnswerAsync(paint.Contact, paint.Platform, yes);
        RepaintArchiveLinksIfMoved();
    }

    /// <summary><c>contact-archive-unlink</c> — future merges stop, claimed content stays
    /// (the shared <c>unlink_token</c>).</summary>
    public async Task UnlinkArchiveLinkAsync(ArchiveLinkRowPaint paint)
    {
        await _links.UnlinkAsync(paint.Contact, paint.Platform);
        RepaintArchiveLinksIfMoved();
    }

    /// <summary>Repaint the knock cards when a status read moved
    /// <see cref="WardAsksCache.KnocksAwaitGuardian"/> (the page calls this on
    /// <see cref="WardAsksCache.Changed"/>), so the cards already shown gain or lose
    /// their <c>knock-guardian-pending</c> without a reload.</summary>
    public void RefreshKnockMarks()
    {
        var awaitsGuardian = WardAsksCache.KnocksAwaitGuardian;
        if (KnockRows.All(r => r.AwaitsGuardian == awaitsGuardian)) return;
        using var names = ContactOverlayNames.Open();
        PaintKnockRows(names);
    }

    private void PaintKnockRows(ContactOverlayNames.Reader names)
    {
        // While the account is supervised with contact approval on, every knock is
        // the guardian's to decide, so each card says so (family-safety.md § App
        // surface) — one shared answer for all cards, no per-knock wire field.
        var awaitsGuardian = WardAsksCache.KnocksAwaitGuardian;
        KnockRows.Clear();
        foreach (var k in Knocks)
            KnockRows.Add(new KnockRow(k, names.PeerLabel(null, null, k.ActorId).primary, awaitsGuardian));
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

    private static ContactRow RowFor(
        ContactOverlayNames.Reader names, ContactInfo c, IReadOnlyList<ArchiveLinkView> linkViews)
    {
        // Roster row: `peer_label(None, handle, id)` (the face's contract). The archive
        // link prompt names the contact by the same primary line the row shows.
        var label = names.PeerLabel(null, c.Handle, c.ActorId);
        return new ContactRow(c, label.primary, label.@public, names.LabelsLine(c.ActorId))
        {
            ArchiveLinks = ArchiveLinksHost.RowPaints(linkViews, c.ActorId, label.primary),
        };
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
    /// <c>user@domain</c> or bare <c>localpart</c> handle | invalid — a bare localpart
    /// is a home-nest handle with an empty <see cref="RecipientClassification.Domain"/>,
    /// forwarded as it is) via the shared
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

    /// <summary>A handle-kind <paramref name="c"/> resolved to its actor id through the
    /// shared find-user lookup (<see cref="FaunaFfiMethods.FindUser"/>, the one
    /// <see cref="LookUpRecipientAsync"/>'s handle branch runs): the typed domain is
    /// forwarded as it is, so a bare localpart's EMPTY domain probes the home nest alone
    /// (contacts.md § Layout & flow region 3) and a qualified handle may resolve on a
    /// peer nest. Returns slot 0 of <c>[actor_id, handle, domain, nest_url]</c>; a miss
    /// throws, as the lookup does. The folder-share picker's handle leg
    /// (<c>FoldersPage.ShareFolderAsync</c>) — never a second resolve chain.
    /// <paramref name="findUser"/> is the unit-test seam (the lookup is a live network
    /// hop).</summary>
    public static async Task<string> FindRecipientActorIdAsync(
        string homeUrl,
        RecipientClassification c,
        Func<string, string, string?, Task<string[]>>? findUser = null)
    {
        var found = await (findUser ?? FaunaFfiMethods.FindUser)(homeUrl, c.User, c.Domain);
        return found[0];
    }

    private readonly INestRpcClient _rpc;

    /// <summary>The Find User lookup seam: <c>(homeUrl, handle, domain)</c> →
    /// <c>[actor_id, handle, domain, nest_url]</c>. Defaults to the shared
    /// <see cref="FaunaFfiMethods.FindUser"/>; a unit test swaps it because the lookup is a
    /// live network hop.</summary>
    internal Func<string, string, string?, Task<string[]>> FindUserAsync { get; set; } =
        FaunaFfiMethods.FindUser;

    internal ContactsViewModel(INestRpcClient rpc, ArchiveLinksHost? archiveLinks = null)
    {
        _rpc = rpc;
        _links = archiveLinks ?? ArchiveLinksHost.Instance;
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
            // Phase two's views are read with the roster (archive-import.md § Linking
            // and merging, ruling 8); a failed read keeps the last ones, and no links
            // wired reads nothing.
            await _links.RefreshAsync();
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
        // typed 64-hex actor-id | user@domain or bare localpart handle (a bare
        // localpart carries an empty domain: the home-nest probe), then resolve it into a single
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
