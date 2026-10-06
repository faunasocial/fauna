using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_core;
using uniffi.fauna_client_config;
using uniffi.fauna_conversations;
using uniffi.fauna_feed;
using uniffi.fauna_log;
using uniffi.fauna_client_capabilities;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="INestRpcClient"/> for view-model unit tests — the
/// WS-RPC-plane peer of <see cref="MockNestHttpClient"/>. Records the kind
/// names called (<see cref="Calls"/>) and returns configurable fixtures. The
/// <c>Make*</c> helpers build the UniFFI reply records (positional, with the
/// uninteresting fields zeroed) so tests don't repeat the field lists. Set
/// <see cref="NextError"/> to make every call throw (error-path tests).
/// </summary>
internal sealed class MockNestRpcClient : INestRpcClient
{
    private readonly List<string> _calls = new();
    public IReadOnlyList<string> Calls => _calls;

    /// <inheritdoc />
    /// <summary>A placeholder home URL; the find-by-handle unit tests exercise the
    /// classify + actor-id/invalid branches, which never open the network resolve
    /// that would read this (the handle resolve is a live anonymous connection,
    /// deferred to e2e).</summary>
    public string HomeUrl { get; set; } = "https://nest.example";

    // ── reconnect re-hydrate ──
    /// <inheritdoc />
    public event Action? Reconnected;
    /// <summary>Test hook: simulate a WS reconnect (raises <see cref="Reconnected"/>
    /// synchronously, as the production pump does on the UI thread).</summary>
    public void RaiseReconnected() => Reconnected?.Invoke();

    /// <inheritdoc />
    public event Action<FfiKnock>? KnockReceived;
    /// <summary>Test hook: simulate an inbound knock push (raises
    /// <see cref="KnockReceived"/> with <paramref name="knock"/>).</summary>
    public void RaiseKnock(FfiKnock knock) => KnockReceived?.Invoke(knock);

    /// <inheritdoc />
    public event Action<string, string>? CalendarPushChanged;
    /// <summary>Test hook: simulate an inbound <c>fauna.calendar.changed</c> push
    /// (raises <see cref="CalendarPushChanged"/> with <paramref name="actorId"/> +
    /// <paramref name="calendarId"/>).</summary>
    public void RaiseCalendarPushChanged(string actorId, string calendarId) =>
        CalendarPushChanged?.Invoke(actorId, calendarId);

    /// <inheritdoc />
    public event Action<string, string>? AddressBookPushChanged;
    /// <summary>Test hook: simulate an inbound <c>fauna.addressbook.changed</c> push
    /// (raises <see cref="AddressBookPushChanged"/> with <paramref name="actorId"/> +
    /// <paramref name="addressbookId"/>).</summary>
    public void RaiseAddressBookPushChanged(string actorId, string addressbookId) =>
        AddressBookPushChanged?.Invoke(actorId, addressbookId);

    /// <inheritdoc />
    public event Action<string>? FolderChangedPushed;
    /// <summary>Test hook: simulate an inbound <c>fauna.sync.changed</c> push
    /// (raises <see cref="FolderChangedPushed"/> with <paramref name="folder"/>).</summary>
    public void RaiseFolderChangedPushed(string folder) =>
        FolderChangedPushed?.Invoke(folder);

    /// <inheritdoc />
    public event Action<FfiConnectionState>? ConnectionStateChanged;
    /// <summary>Test hook: simulate a transport connection-state transition (raises
    /// <see cref="ConnectionStateChanged"/> synchronously, as the production pump
    /// does on the UI thread).</summary>
    public void RaiseConnectionState(FfiConnectionState state) => ConnectionStateChanged?.Invoke(state);
    /// <inheritdoc />
    public void StartConnectionStatePump() => _calls.Add("StartConnectionStatePump");

    // ── Configurable replies ──
    public IdentityInfo? NextIdentity { get; set; }
    public FfiQuotaGetReply? NextQuota { get; set; }
    // ── Configurable encrypted-CalDAV replies ──
    public IReadOnlyList<FfiCalendarRow> NextCaldavCalendars { get; set; } = new List<FfiCalendarRow>();
    public IReadOnlyList<FfiCalEvent> NextCaldavEvents { get; set; } = new List<FfiCalEvent>();
    public IReadOnlyList<FfiCalEvent> NextCaldavInvited { get; set; } = new List<FfiCalEvent>();
    /// <summary>Per-calendar-id override for <see cref="CaldavQueryEventsAsync"/>, read
    /// AFTER <see cref="CaldavQueryEventsHook"/> completes — lets a test give two
    /// concurrent calls (e.g. a gated union fan-out vs. an ungated scoped query)
    /// distinct, order-independent results. Falls back to <see cref="NextCaldavEvents"/>
    /// for an id with no entry.</summary>
    public Dictionary<string, IReadOnlyList<FfiCalEvent>>? CaldavQueryEventsById { get; set; }
    /// <summary>Per-calendar-id override for <see cref="CaldavQueryEventsSeededAsync"/>:
    /// a <c>null</c> value means "the backstop probe says unchanged" (the call
    /// returns <c>null</c>); a list means "changed" (returns that list). An id with
    /// no entry falls back to <see cref="NextCaldavEvents"/>, matching the full
    /// read's own default.</summary>
    public Dictionary<string, IReadOnlyList<FfiCalEvent>?>? CaldavQueryEventsSeededById { get; set; }
    /// <summary>Awaited before every <see cref="CaldavQueryEventsAsync"/> reply — lets a
    /// test suspend one specific call (by calendar id) mid-flight to reproduce the
    /// stale-query-overwrites-newer-selection race (events.md § Implementation status
    /// today, the no-selection-union row) deterministically.</summary>
    public Func<string, Task>? CaldavQueryEventsHook { get; set; }
    public FfiCalEvent? NextCaldavEvent { get; set; }
    public IReadOnlyList<BridgeInfo> NextBridges { get; set; } = new List<BridgeInfo>();
    public IReadOnlyList<BridgeFollow> NextFollows { get; set; } = new List<BridgeFollow>();
    public IReadOnlyList<BridgeFeedSubscription> NextFeeds { get; set; } = new List<BridgeFeedSubscription>();
    public IReadOnlyList<ConflictInfo> NextConflicts { get; set; } = new List<ConflictInfo>();
    public bool NextAmIAdmin { get; set; }
    /// <summary>Reply for <see cref="ReportHostAddressAsync"/> — defaults to the
    /// safe LAN-box floor (<c>SkippedNoPublicIp</c>).</summary>
    public FfiHostAddressOutcome NextHostAddressOutcome { get; set; } =
        new FfiHostAddressOutcome.SkippedNoPublicIp();
    /// <summary>When set, <see cref="ReportHostAddressAsync"/> throws it (the
    /// report-path fault the reporter must swallow), independent of the gate.</summary>
    public string? NextReportHostAddressError { get; set; }
    public FfiAdminStats? NextAdminStats { get; set; }
    public FfiAdminStatus? NextAdminStatus { get; set; }
    public string? NextError { get; set; }

    // ── Configurable admin replies (FFI returns arrays = IReadOnlyList<T>) ──
    public FfiAdminUser[] NextAdminUsers { get; set; } = Array.Empty<FfiAdminUser>();
    public long NextAdminUsersTotal { get; set; }
    public FfiAdminTier[] NextAdminTiers { get; set; } = Array.Empty<FfiAdminTier>();
    public FfiAdminInviteCode[] NextAdminInviteCodes { get; set; } = Array.Empty<FfiAdminInviteCode>();
    public FfiAdminInviteRequest[] NextAdminInviteRequests { get; set; } = Array.Empty<FfiAdminInviteRequest>();
    public string NextMintedInviteCode { get; set; } = "MINTED-CODE";

    // ── Captured arguments (last call) ──
    /// Last <c>invite_attendee</c> email.
    public string? LastInviteEmail { get; private set; }
    /// Last <c>rsvp_event</c> response.
    public uniffi.fauna_core.RsvpResponse? LastRsvpResponse { get; private set; }
    /// Last <c>set_reminder</c> offset (<c>""</c> = cleared).
    public string? LastReminderOffset { get; private set; }
    /// Last <c>create_event</c> calendar id + summary.
    public (string CalendarIdHex, string Summary)? LastCreatedEvent { get; private set; }

    /// Last <c>fauna.admin.users.update</c> args, for asserting tier changes.
    public (byte[] ActorId, string Tier, string Label)? LastAdminUserUpdate { get; private set; }
    /// Last <c>fauna.admin.users.evict</c> args (asserts one-click default reason + "other").
    public (byte[] ActorId, string Reason, string Category)? LastAdminUserEvict { get; private set; }
    /// Last <c>fauna.admin.users.suspend</c> args (asserts one-click default reason + "other").
    public (byte[] ActorId, string Reason, string Category)? LastAdminUserSuspend { get; private set; }
    /// Last <c>fauna.admin.users.cancel_eviction</c> actor id.
    public byte[]? LastAdminUserCancelEviction { get; private set; }
    /// Last <c>fauna.admin.admins.add</c> actor id.
    public byte[]? LastAdminAdminsAdd { get; private set; }
    /// Last <c>fauna.admin.admins.remove</c> actor id.
    public byte[]? LastAdminAdminsRemove { get; private set; }
    /// Last <c>fauna.admin.invite_codes.create</c> args (asserts mint-on-empty).
    public (string Code, string Tier, long Uses)? LastAdminInviteCreate { get; private set; }
    /// Last <c>fauna.admin.invite_codes.create</c> guardian_actor (family-safety.md § Wire &amp; data shape).
    public byte[]? LastAdminInviteCreateGuardianActor { get; private set; }
    /// Last <c>fauna.admin.invite_requests.approve</c> args (asserts approve-at-tier).
    public (long Id, string? Tier, string? Label)? LastAdminApprove { get; private set; }
    /// Last <c>fauna.admin.invite_requests.approve</c> guardian_actor (family-safety.md § Wire &amp; data shape).
    public byte[]? LastAdminApproveGuardianActor { get; private set; }
    /// Last <c>fauna.admin.invite_codes.create</c> age_band (family-safety.md § App surface → *Age-band surfaces*).
    public string? LastAdminInviteCreateAgeBand { get; private set; }
    /// Last <c>fauna.admin.invite_requests.approve</c> age_band.
    public string? LastAdminApproveAgeBand { get; private set; }
    /// Last <c>fauna.admin.set_age_verification_required</c> value; <c>null</c> = never sent.
    public bool? LastAgeVerificationRequiredSet { get; private set; }

    /// Last <c>fauna.bridges.link</c> arguments, for asserting the composed call.
    public (string BridgeId, string Mode, IReadOnlyDictionary<string, string> Fields)? LastLink { get; private set; }
    public (string BridgeId, IReadOnlyList<BridgeSettingValue> Settings)? LastSetSettings { get; private set; }
    /// <summary>When false, <c>set_settings</c> is recorded but NOT applied to
    /// <see cref="NextBridges"/> — lets a test prove a caller re-reads rather than
    /// echoing its own request.</summary>
    public bool PersistSetSettings { get; set; } = true;

    /// <summary>Row id for the next minted invite; incremented per mint.</summary>
    public long NextBunkerConnectionId { get; set; } = 1;
#if PAYMENTS
    /// <summary>The zap-signers roster this mock actually keeps. <c>add</c>
    /// upserts (by pubkey, matching the nest's own idempotent-designate rule)
    /// and <c>remove</c> deletes, so a view model that painted its own
    /// optimistic guess instead of re-reading would fail the round-trip
    /// assertions — the same non-vacuity property <see cref="PersistSetSettings"/>
    /// gives the settings plane.</summary>
    public List<ZapSignerEntry> ZapSignerRoster { get; } = new();
    /// <summary>Row id for the next designated signer; incremented per add.</summary>
    public long NextZapSignerId { get; set; } = 1;
#endif

    /// Configurable <c>fauna.conversations.keypackage.count</c> reply.
    public int NextKeypackageCount { get; set; }
    /// Last <c>fauna.conversations.keypackage.upload</c> packages (raw KP bytes).
    public IReadOnlyList<byte[]>? LastUploadedKeyPackages { get; private set; }
    /// Last <c>fauna.conversations.keypackage.upload</c> last-resort flag.
    public bool LastUploadLastResort { get; private set; }

    private void Throw()
    {
        if (NextError is not null) throw new InvalidOperationException(NextError);
    }

    // ── fauna.sync.conflicts.* ──────────────────────────────────────────

    public Task<IReadOnlyList<ConflictInfo>> ConflictsListAsync()
    {
        _calls.Add("ConflictsList");
        Throw();
        return Task.FromResult(NextConflicts);
    }

    // ── fauna.account.am_i_admin / fauna.admin.stats ────────────────────

    public Task<bool> AmIAdminAsync()
    {
        _calls.Add("AmIAdmin");
        Throw();
        return Task.FromResult(NextAmIAdmin);
    }

    public Task<FfiHostAddressOutcome> ReportHostAddressAsync()
    {
        _calls.Add("ReportHostAddress");
        if (NextReportHostAddressError is not null)
            throw new InvalidOperationException(NextReportHostAddressError);
        Throw();
        return Task.FromResult(NextHostAddressOutcome);
    }

    public Task<FfiAdminStats> AdminStatsAsync()
    {
        _calls.Add("AdminStats");
        Throw();
        return Task.FromResult(NextAdminStats ?? MakeAdminStats());
    }

    public Task<FfiAdminStatus> AdminStatusAsync()
    {
        _calls.Add("AdminStatus");
        Throw();
        return Task.FromResult(NextAdminStatus ?? MakeAdminStatus());
    }

    // ── encrypted CalDAV store (FfiCaldavClient) ────────────────────────

    public Task<IReadOnlyList<FfiCalendarRow>> CaldavListCalendarsAsync()
    {
        _calls.Add("CaldavListCalendars");
        Throw();
        return Task.FromResult(NextCaldavCalendars);
    }

    public Task CaldavCreateCalendarAsync(string name)
    {
        _calls.Add("CaldavCreateCalendar");
        Throw();
        return Task.CompletedTask;
    }

    public async Task<IReadOnlyList<FfiCalEvent>> CaldavQueryEventsAsync(string calendarIdHex)
    {
        _calls.Add("CaldavQueryEvents");
        if (CaldavQueryEventsHook is { } hook) await hook(calendarIdHex);
        Throw();
        return CaldavQueryEventsById is { } byId && byId.TryGetValue(calendarIdHex, out var forId)
            ? forId
            : NextCaldavEvents;
    }

    public Task<IReadOnlyList<FfiCalEvent>> CaldavQueryInvitedEventsAsync()
    {
        _calls.Add("CaldavQueryInvitedEvents");
        Throw();
        return Task.FromResult(NextCaldavInvited);
    }

    public Task<IReadOnlyList<FfiCalEvent>?> CaldavQueryEventsSeededAsync(string calendarIdHex)
    {
        _calls.Add("CaldavQueryEventsSeeded");
        Throw();
        if (CaldavQueryEventsSeededById is { } byId && byId.TryGetValue(calendarIdHex, out var forId))
            return Task.FromResult(forId);
        return Task.FromResult<IReadOnlyList<FfiCalEvent>?>(NextCaldavEvents);
    }

    public Task<FfiCalEvent?> CaldavGetEventAsync(string uidHashHex)
    {
        _calls.Add("CaldavGetEvent");
        Throw();
        return Task.FromResult(NextCaldavEvent);
    }

    public Task CaldavCreateEventAsync(
        string calendarIdHex, string summary, string dtstart, string dtend, string? location, string? description)
    {
        _calls.Add("CaldavCreateEvent");
        LastCreatedEvent = (calendarIdHex, summary);
        Throw();
        return Task.CompletedTask;
    }

    public Task CaldavDeleteEventAsync(string uidHashHex)
    {
        _calls.Add("CaldavDeleteEvent");
        Throw();
        return Task.CompletedTask;
    }

    public Task CaldavRsvpEventAsync(string uidHashHex, uniffi.fauna_core.RsvpResponse response)
    {
        _calls.Add("CaldavRsvpEvent");
        LastRsvpResponse = response;
        Throw();
        return Task.CompletedTask;
    }

    public Task CaldavSetReminderAsync(string uidHashHex, string offset)
    {
        _calls.Add("CaldavSetReminder");
        LastReminderOffset = offset;
        Throw();
        return Task.CompletedTask;
    }

    public Task CaldavInviteAttendeeAsync(string uidHashHex, string email)
    {
        _calls.Add("CaldavInviteAttendee");
        LastInviteEmail = email;
        Throw();
        return Task.CompletedTask;
    }

    // ── fauna.conversations.keypackage.* ────────────────────────────────

    public Task<int> KeypackageCountAsync()
    {
        _calls.Add("KeypackageCount");
        Throw();
        return Task.FromResult(NextKeypackageCount);
    }

    public Task<uint> KeypackageUploadAsync(IReadOnlyList<byte[]> packages, bool lastResort = false)
    {
        _calls.Add("KeypackageUpload");
        LastUploadedKeyPackages = packages;
        LastUploadLastResort = lastResort;
        Throw();
        return Task.FromResult((uint)packages.Count);
    }

    // ── fauna.account.* / fauna.quota.get ───────────────────────────────

    public Task<IdentityInfo?> GetIdentityAsync()
    {
        _calls.Add("GetIdentity");
        Throw();
        return Task.FromResult(NextIdentity);
    }

    public Task<FfiQuotaGetReply> QuotaGetAsync()
    {
        _calls.Add("QuotaGet");
        Throw();
        return Task.FromResult(NextQuota ?? MakeQuota(0, 0));
    }

    /// The `features.rows` fixture — defaults to an empty registry (no gated
    /// features at all).
    public IReadOnlyList<FfiFeatureRow> NextFeatureRows { get; set; } = Array.Empty<FfiFeatureRow>();

    public Task<bool> RefreshRegionPlaneAsync(FfiRegionPlane plane, bool onlyIfDue)
    {
        _calls.Add(onlyIfDue ? "RefreshRegionPlaneIfDue" : "RefreshRegionPlane");
        Throw();
        return Task.FromResult(false);
    }

    public Task<IReadOnlyList<FfiFeatureRow>> FeaturesRowsAsync()
    {
        _calls.Add("FeaturesRows");
        Throw();
        return Task.FromResult(NextFeatureRows);
    }

    public Task AccountDeleteAsync()
    {
        _calls.Add("AccountDelete");
        Throw();
        return Task.CompletedTask;
    }

    public Task ChangeHandleAsync(string handle)
    {
        _calls.Add("ChangeHandle");
        Throw();
        return Task.CompletedTask;
    }

    /// <summary>Configurable fixture for <see cref="PendingActionsListAsync"/> —
    /// already narrowed to <c>pending</c> rows, matching what the production
    /// implementation returns.</summary>
    public IReadOnlyList<FfiPendingActionSummary> NextPendingActions { get; set; } =
        Array.Empty<FfiPendingActionSummary>();

    public Task<IReadOnlyList<FfiPendingActionSummary>> PendingActionsListAsync()
    {
        _calls.Add("PendingActionsList");
        Throw();
        return Task.FromResult(NextPendingActions);
    }

    public Task PendingActionCancelAsync(long id)
    {
        _calls.Add("PendingActionCancel");
        Throw();
        return Task.CompletedTask;
    }

    // ── fauna.bridges.* ─────────────────────────────────────────────────

    public Task<IReadOnlyList<BridgeInfo>> BridgesListAsync()
    {
        _calls.Add("BridgesList");
        Throw();
        return Task.FromResult(NextBridges);
    }

    public Task BridgesLinkAsync(string bridgeId, string mode, IReadOnlyDictionary<string, string> fields)
    {
        _calls.Add("BridgesLink");
        LastLink = (bridgeId, mode, fields);
        return Task.CompletedTask;
    }

    public Task BridgesUnlinkAsync(string bridgeId)
    {
        _calls.Add("BridgesUnlink");
        return Task.CompletedTask;
    }

    /// <summary>Records the write AND applies it to <see cref="NextBridges"/>, so a
    /// caller that re-reads after writing observes the persisted value. That is what
    /// makes a non-optimistic round-trip assertion non-vacuous: a view model that
    /// merely echoed the requested value would pass against a record-only mock.</summary>
    public Task BridgesSetSettingsAsync(string bridgeId, IReadOnlyList<BridgeSettingValue> settings)
    {
        _calls.Add("BridgesSetSettings");
        LastSetSettings = (bridgeId, settings);
        Throw();
        // A nest that accepts the write but keeps reporting the old value — the shape
        // that separates a view model which RE-READS from one which echoes the request.
        if (!PersistSetSettings) return Task.CompletedTask;
        NextBridges = NextBridges
            .Select(b => b.Id != bridgeId ? b : b with { Settings = Merge(b.Settings, settings) })
            .ToList();
        return Task.CompletedTask;
    }

    private static IReadOnlyList<BridgeSetting> Merge(
        IReadOnlyList<BridgeSetting> existing, IReadOnlyList<BridgeSettingValue> writes)
    {
        var merged = existing.ToList();
        foreach (var w in writes)
        {
            var i = merged.FindIndex(s => s.Key == w.Key);
            var row = new BridgeSetting(
                w.Key,
                i >= 0 ? merged[i].Label : w.Key,
                w.BoolValue is not null ? "bool" : w.NumberValue is not null ? "number" : "text",
                w.BoolValue,
                w.TextValue,
                w.NumberValue);
            if (i >= 0) merged[i] = row; else merged.Add(row);
        }
        return merged;
    }

    public Task<IReadOnlyList<BridgeFollow>> BridgesListFollowsAsync(string bridgeId)
    {
        _calls.Add("BridgesListFollows");
        return Task.FromResult(NextFollows);
    }

    public Task BridgesAddFollowAsync(string bridgeId, string id, string? petname)
    {
        _calls.Add("BridgesAddFollow");
        return Task.CompletedTask;
    }

    public Task BridgesRemoveFollowAsync(string bridgeId, string followId)
    {
        _calls.Add("BridgesRemoveFollow");
        return Task.CompletedTask;
    }

    public Task<IReadOnlyList<BridgeFeedSubscription>> BridgesFeedsListAsync()
    {
        _calls.Add("BridgesFeedsList");
        return Task.FromResult(NextFeeds);
    }

    public Task BridgesFeedsCreateAsync(string bridge, string feedUri, string name)
    {
        _calls.Add("BridgesFeedsCreate");
        return Task.CompletedTask;
    }

    // ── fauna.nostr.bunker.* ────────────────────────────────────────────

    /// <summary>Returns the one-time reveal, shaped like the nest's (<c>bunker://&lt;signer&gt;?relay=…&amp;secret=…</c>)
    /// so a caller asserting on the string's shape is asserting on something real.</summary>
    public Task<BunkerInvite> NostrBunkerCreateInviteAsync()
    {
        _calls.Add("NostrBunkerCreateInvite");
        Throw();
        var id = NextBunkerConnectionId++;
        return Task.FromResult(new BunkerInvite(
            id,
            $"bunker://signerpub{id}?relay=wss://nest.example/nostr&secret=onetime{id}",
            $"signerpub{id}",
            2000));
    }

    // ── succession-aftermath npub confirm ────────────────────────────────

    /// <summary>Default: not owed (the ordinary case — no pending
    /// succession-aftermath confirmation).</summary>
    public bool NextNpubConfirmationOwed { get; set; }

    /// <summary>Set by <see cref="ConfirmNpubAsync"/>, so a test can assert the
    /// confirm write actually ran and clear <see cref="NextNpubConfirmationOwed"/>
    /// itself (this mock does not auto-clear it — the VM's own re-read after the
    /// write is what a real nest would answer, and tests configure that directly).
    /// </summary>
    public int ConfirmNpubCallCount { get; private set; }

    public Task<bool> NpubConfirmationOwedAsync()
    {
        _calls.Add("NpubConfirmationOwed");
        Throw();
        return Task.FromResult(NextNpubConfirmationOwed);
    }

    public Task ConfirmNpubAsync()
    {
        _calls.Add("ConfirmNpub");
        ConfirmNpubCallCount++;
        Throw();
        return Task.CompletedTask;
    }

#if PAYMENTS
    public Task<IReadOnlyList<ZapSignerEntry>> NostrZapSignersListAsync()
    {
        _calls.Add("NostrZapSignersList");
        Throw();
        return Task.FromResult<IReadOnlyList<ZapSignerEntry>>(ZapSignerRoster.ToList());
    }

    /// <summary>Idempotent by pubkey, mirroring the nest's own designate rule
    /// (re-adding a designated signer refreshes its label) — and NORMALIZES to
    /// lowercase, so a test typing an uppercase pubkey exercises the real
    /// STORED-not-typed render contract exactly as the live nest does.</summary>
    public Task<ZapSignerEntry> NostrZapSignersAddAsync(string signerPubkey, string label)
    {
        _calls.Add("NostrZapSignersAdd");
        Throw();
        var stored = signerPubkey.ToLowerInvariant();
        var existing = ZapSignerRoster.FindIndex(s => s.SignerPubkey == stored);
        var entry = new ZapSignerEntry(
            existing >= 0 ? ZapSignerRoster[existing].Id : NextZapSignerId++,
            stored, label, 1000);
        if (existing >= 0) ZapSignerRoster[existing] = entry;
        else ZapSignerRoster.Add(entry);
        return Task.FromResult(entry);
    }

    public Task<bool> NostrZapSignersRemoveAsync(string signerPubkey)
    {
        _calls.Add("NostrZapSignersRemove");
        Throw();
        return Task.FromResult(ZapSignerRoster.RemoveAll(s => s.SignerPubkey == signerPubkey) > 0);
    }
#endif

    public Task BridgesFeedsDeleteAsync(long id)
    {
        _calls.Add("BridgesFeedsDelete");
        return Task.CompletedTask;
    }

    // ── fauna.email.* ───────────────────────────────────────────────────

    public IReadOnlyList<FfiEmailFilter> NextFilters { get; set; } = new List<FfiEmailFilter>();

    public Task<IReadOnlyList<FfiEmailFilter>> EmailFiltersListAsync()
    {
        _calls.Add("EmailFiltersList");
        return Task.FromResult(NextFilters);
    }

    public Task EmailFiltersCreateAsync(
        string name, IReadOnlyList<FfiEmailFilterRule> rules,
        string combination, FfiEmailFilterAction action, int priority)
    {
        _calls.Add("EmailFiltersCreate");
        return Task.CompletedTask;
    }

    public Task EmailFiltersDeleteAsync(long id)
    {
        _calls.Add("EmailFiltersDelete");
        return Task.CompletedTask;
    }

    /// The single-row fixture <see cref="EmailFiltersGetAsync"/> returns (the edit
    /// dialog's fresh fetch). Defaults to a single-rule/Allow row (the editable
    /// shape) so tests that don't care about the exact content still exercise
    /// the edit-populate path.
    public FfiEmailFilter NextFilter { get; set; } = new(
        1, "filter", new[] { new FfiEmailFilterRule.SenderIs("someone@example.com") },
        "all", new FfiEmailFilterAction.Allow(), 0, 0);
    /// Last <c>fauna.email.filters.get</c> id.
    public long? LastFilterGetId { get; private set; }
    /// Last <c>fauna.email.filters.update</c> args.
    public (long Id, string Name, IReadOnlyList<FfiEmailFilterRule> Rules, string Combination, FfiEmailFilterAction Action, int Priority)? LastFilterUpdate { get; private set; }

    public Task<FfiEmailFilter> EmailFiltersGetAsync(long id)
    {
        _calls.Add("EmailFiltersGet");
        LastFilterGetId = id;
        Throw();
        return Task.FromResult(NextFilter);
    }

    public Task EmailFiltersUpdateAsync(
        long id, string name, IReadOnlyList<FfiEmailFilterRule> rules,
        string combination, FfiEmailFilterAction action, int priority)
    {
        _calls.Add("EmailFiltersUpdate");
        LastFilterUpdate = (id, name, rules, combination, action, priority);
        Throw();
        return Task.CompletedTask;
    }

    // ── fauna.knocks.* / fauna.contacts.* / fauna.inbox.mode.* ──────────

    public IReadOnlyList<KnockInfo> NextKnocks { get; set; } = new List<KnockInfo>();
    public IReadOnlyList<ContactInfo> NextContacts { get; set; } = new List<ContactInfo>();
    public string NextInboxMode { get; set; } = "allow_knock";

    /// Last <c>fauna.inbox.mode.set</c> mode, for asserting the composed call.
    public string? LastInboxModeSet { get; private set; }
    /// Last knock peer_id passed to accept/block/dismiss.
    public string? LastKnockPeer { get; private set; }
    /// Last actor id passed to <c>fauna.inbox.send</c> (add-contact knock).
    public string? LastKnockSent { get; private set; }

    public Task<IReadOnlyList<KnockInfo>> KnocksListAsync()
    {
        _calls.Add("KnocksList");
        Throw();
        return Task.FromResult(NextKnocks);
    }

    public Task KnocksAcceptAsync(string peerId)
    {
        _calls.Add("KnocksAccept");
        LastKnockPeer = peerId;
        Throw();
        return Task.CompletedTask;
    }

    public Task KnocksBlockAsync(string peerId)
    {
        _calls.Add("KnocksBlock");
        LastKnockPeer = peerId;
        Throw();
        return Task.CompletedTask;
    }

    public Task KnocksUnblockAsync(string peerId)
    {
        _calls.Add("KnocksUnblock");
        LastKnockPeer = peerId;
        Throw();
        return Task.CompletedTask;
    }

    public Task KnocksDismissAsync(string peerId)
    {
        _calls.Add("KnocksDismiss");
        LastKnockPeer = peerId;
        Throw();
        return Task.CompletedTask;
    }

    public Task<IReadOnlyList<ContactInfo>> ContactsListAsync()
    {
        _calls.Add("ContactsList");
        Throw();
        return Task.FromResult(NextContacts);
    }

    /// Last `fauna.contacts.confirm` peer id hex.
    public string? LastContactsConfirmPeer { get; private set; }

    public Task ContactsConfirmAsync(string peerId)
    {
        _calls.Add("ContactsConfirm");
        LastContactsConfirmPeer = peerId;
        Throw();
        return Task.CompletedTask;
    }

    public Task SendKnockAsync(string actorId)
    {
        _calls.Add("SendKnock");
        LastKnockSent = actorId;
        Throw();
        return Task.CompletedTask;
    }

    public Task<string> InboxModeGetAsync()
    {
        _calls.Add("InboxModeGet");
        Throw();
        return Task.FromResult(NextInboxMode);
    }

    public Task InboxModeSetAsync(string mode)
    {
        _calls.Add("InboxModeSet");
        LastInboxModeSet = mode;
        Throw();
        return Task.CompletedTask;
    }

    // ── fauna.notifications.* ───────────────────────────────────────────

    public FfiNotifListReply NextNotifications { get; set; } = FfiNotifListReplyFixture.Make();
    public long NextUnreadCount { get; set; }

    public Task<FfiNotifListReply> NotificationsListAsync(long? cursor, long? limit)
    {
        _calls.Add("NotificationsList");
        Throw();
        return Task.FromResult(NextNotifications);
    }

    public Task<long> NotificationsMarkReadAsync(long? upTo)
    {
        _calls.Add("NotificationsMarkRead");
        Throw();
        return Task.FromResult(0L);
    }

    public Task<long> NotificationsCountAsync()
    {
        _calls.Add("NotificationsCount");
        Throw();
        return Task.FromResult(NextUnreadCount);
    }

    // ── fauna.setup.status ──────────────────────────────────────────────

    /// The <c>fauna.setup.status</c> fixture, backing the <c>admin-nest</c> page's
    /// serving-port / router-fronted / host-OS-maintenance reads. Defaults to an
    /// unset mode (null) — the <c>mode</c> field itself is still on the wire (a
    /// transition-window artifact of the no-modes cutover, Phase-4 S8.7) but no
    /// client renders it anymore.
    public FfiSetupStatus NextSetupStatus { get; set; } = MakeSetupStatus();

    public Task<FfiSetupStatus> SetupStatusAsync()
    {
        _calls.Add("SetupStatus");
        Throw();
        return Task.FromResult(NextSetupStatus);
    }

    // ── fauna.admin.* ────────────────────────────────────────────────────

    public Task<FfiAdminUsersListReply> AdminUsersListAsync(long? limit, long offset)
    {
        _calls.Add("AdminUsersList");
        Throw();
        return Task.FromResult(MakeAdminUsersListReply(NextAdminUsers, NextAdminUsersTotal));
    }

    public Task<FfiAdminUser[]> AdminUsersListAllAsync()
    {
        _calls.Add("AdminUsersListAll");
        Throw();
        return Task.FromResult(NextAdminUsers);
    }

    public Task AdminUsersUpdateAsync(byte[] actorId, string tier, string label)
    {
        _calls.Add("AdminUsersUpdate");
        LastAdminUserUpdate = (actorId, tier, label);
        Throw();
        return Task.CompletedTask;
    }

    public Task AdminUsersEvictAsync(byte[] actorId, string reason, string category)
    {
        _calls.Add("AdminUsersEvict");
        LastAdminUserEvict = (actorId, reason, category);
        Throw();
        return Task.CompletedTask;
    }

    public Task AdminUsersSuspendAsync(byte[] actorId, string reason, string category)
    {
        _calls.Add("AdminUsersSuspend");
        LastAdminUserSuspend = (actorId, reason, category);
        Throw();
        return Task.CompletedTask;
    }

    public Task AdminUsersCancelEvictionAsync(byte[] actorId)
    {
        _calls.Add("AdminUsersCancelEviction");
        LastAdminUserCancelEviction = actorId;
        Throw();
        return Task.CompletedTask;
    }

    public Task AdminAdminsAddAsync(byte[] actorId)
    {
        _calls.Add("AdminAdminsAdd");
        LastAdminAdminsAdd = actorId;
        Throw();
        return Task.CompletedTask;
    }

    public Task AdminAdminsRemoveAsync(byte[] actorId)
    {
        _calls.Add("AdminAdminsRemove");
        LastAdminAdminsRemove = actorId;
        Throw();
        return Task.CompletedTask;
    }

    public Task<IReadOnlyList<FfiAdminUser>> AdminEvictionsListAsync()
    {
        _calls.Add("AdminEvictionsList");
        Throw();
        return Task.FromResult<IReadOnlyList<FfiAdminUser>>(NextAdminUsers);
    }

    public Task<IReadOnlyList<FfiAdminTier>> AdminTiersListAsync()
    {
        _calls.Add("AdminTiersList");
        Throw();
        return Task.FromResult<IReadOnlyList<FfiAdminTier>>(NextAdminTiers);
    }

    // ── fauna.admin.membership_tiers.* (monetization.md § Pillar 4) ──────

    /// The designations `membership_tiers.list` returns — which owned subscription
    /// tiers are designated. Independent of <see cref="NextTiers"/> (the row set),
    /// so a test can render an owned-but-undesignated row.
    public FfiAdminMembershipTier[] NextMembershipTiers { get; set; }
        = Array.Empty<FfiAdminMembershipTier>();

    /// Last `fauna.admin.membership_tiers.set` args — asserts the upsert carries the
    /// row's *current* selection plus both linked quota tiers.
    public (string TierName, string AdminTier, string LapseTier)? LastMembershipSet { get; private set; }

    /// Last `fauna.admin.membership_tiers.clear` tier name.
    public string? LastMembershipClear { get; private set; }

    public Task<IReadOnlyList<FfiAdminMembershipTier>> AdminMembershipTiersListAsync()
    {
        _calls.Add("AdminMembershipTiersList");
        Throw();
        return Task.FromResult<IReadOnlyList<FfiAdminMembershipTier>>(NextMembershipTiers);
    }

    public Task AdminMembershipTiersSetAsync(string tierName, string adminTier, string lapseTier)
    {
        _calls.Add("AdminMembershipTiersSet");
        Throw();
        LastMembershipSet = (tierName, adminTier, lapseTier);
        return Task.CompletedTask;
    }

    public Task AdminMembershipTiersClearAsync(string tierName)
    {
        _calls.Add("AdminMembershipTiersClear");
        Throw();
        LastMembershipClear = tierName;
        return Task.CompletedTask;
    }

    /// Last <c>fauna.admin.tiers.update</c> args (name + the five raw-i64 caps).
    public (string Name, long Inbox, long Storage, long Devices, long BlobSize, long Feeds)? LastTierUpdate { get; private set; }

    public Task AdminTiersUpdateAsync(
        string name, long maxInboxBytes, long maxStorageBytes, long maxDevices, long maxBlobSize, long maxFeeds)
    {
        _calls.Add("AdminTiersUpdate");
        LastTierUpdate = (name, maxInboxBytes, maxStorageBytes, maxDevices, maxBlobSize, maxFeeds);
        Throw();
        return Task.CompletedTask;
    }

    public (FfiRegistrationMode mode, ulong? maxFreeUsers)? LastRegistrationModeSet { get; private set; }

    public Task AdminSetRegistrationModeAsync(FfiRegistrationMode mode, ulong? maxFreeUsers)
    {
        _calls.Add("AdminSetRegistrationMode");
        LastRegistrationModeSet = (mode, maxFreeUsers);
        Throw();
        return Task.CompletedTask;
    }

    public Task AdminSetAgeVerificationRequiredAsync(bool required)
    {
        _calls.Add("AdminSetAgeVerificationRequired");
        LastAgeVerificationRequiredSet = required;
        Throw();
        return Task.CompletedTask;
    }

    public (byte[] actorId, string tier, string? handle)? LastUsersCreate { get; private set; }

    public Task AdminUsersCreateAsync(byte[] actorId, string tier, string? handle)
    {
        _calls.Add("AdminUsersCreate");
        LastUsersCreate = (actorId, tier, handle);
        Throw();
        return Task.CompletedTask;
    }

    public Task<IReadOnlyList<FfiAdminInviteCode>> AdminInviteCodesListAsync()
    {
        _calls.Add("AdminInviteCodesList");
        Throw();
        return Task.FromResult<IReadOnlyList<FfiAdminInviteCode>>(NextAdminInviteCodes);
    }

    public Task<string> AdminInviteCodesCreateAsync(string code, string tier, long uses, byte[]? guardianActorId = null, string? ageBand = null)
    {
        _calls.Add("AdminInviteCodesCreate");
        LastAdminInviteCreate = (code, tier, uses);
        LastAdminInviteCreateGuardianActor = guardianActorId;
        LastAdminInviteCreateAgeBand = ageBand;
        Throw();
        // Mint-on-empty: empty code ⇒ nest mints one and returns it (admin.md § 3).
        return Task.FromResult(string.IsNullOrEmpty(code) ? NextMintedInviteCode : code);
    }

    public Task AdminInviteCodesDeleteAsync(string code)
    {
        _calls.Add("AdminInviteCodesDelete");
        Throw();
        return Task.CompletedTask;
    }

    public Task<IReadOnlyList<FfiAdminInviteRequest>> AdminInviteRequestsListAsync()
    {
        _calls.Add("AdminInviteRequestsList");
        Throw();
        return Task.FromResult<IReadOnlyList<FfiAdminInviteRequest>>(NextAdminInviteRequests);
    }

    public Task<FfiAdminInviteRequestApproveReply> AdminInviteRequestsApproveAsync(long id, string? tier, string? label, byte[]? guardianActorId = null, string? ageBand = null)
    {
        _calls.Add("AdminInviteRequestsApprove");
        LastAdminApprove = (id, tier, label);
        LastAdminApproveGuardianActor = guardianActorId;
        LastAdminApproveAgeBand = ageBand;
        Throw();
        return Task.FromResult(MakeAdminInviteRequestApproveReply(tier: tier ?? ""));
    }

    public Task AdminInviteRequestsDenyAsync(long id, string? reason)
    {
        _calls.Add("AdminInviteRequestsDeny");
        Throw();
        return Task.CompletedTask;
    }

    /// The <c>fauna.admin.services.list</c> flags fixture (bridge/pairing).
    public FfiAdminServiceFlags NextServiceFlags { get; set; } = new(false, false);
    /// Last <c>fauna.admin.services.update</c> args, for asserting a toggle flip.
    public (string Name, bool Enabled)? LastServiceUpdate { get; private set; }

    public Task<FfiAdminServiceFlags> AdminServicesListAsync()
    {
        _calls.Add("AdminServicesList");
        Throw();
        return Task.FromResult(NextServiceFlags);
    }

    public Task AdminServicesUpdateAsync(string name, bool enabled)
    {
        _calls.Add("AdminServicesUpdate");
        LastServiceUpdate = (name, enabled);
        Throw();
        return Task.CompletedTask;
    }

    /// Last <c>fauna.admin.set_serving_port</c> port, for asserting the node-policy write.
    public ushort? LastServingPort { get; private set; }

    public Task SetServingPortAsync(ushort port)
    {
        _calls.Add("SetServingPort");
        LastServingPort = port;
        Throw();
        return Task.CompletedTask;
    }

    /// Whether <c>fauna.admin.request_host_restart</c> was dispatched (the admin
    /// "restart now" on admin-nest).
    public bool HostRestartRequested { get; private set; }

    public Task RequestHostRestartAsync()
    {
        _calls.Add("RequestHostRestart");
        HostRestartRequested = true;
        Throw();
        return Task.CompletedTask;
    }

    /// The <c>fauna.admin.region.get</c> fixture (undeclared by default).
    public FfiAdminRegionView NextRegionView { get; set; } = new(
        null, new LocalizedText("admin.nest_page.region_none", new()), null, null, false);
    /// Every <c>fauna.admin.region.set</c> arg, in call order (<c>null</c> = withdraw).
    public List<string?> RegionSetCalls { get; } = new();

    public Task<FfiAdminRegionView> AdminRegionStatusAsync()
    {
        _calls.Add("AdminRegionStatus");
        Throw();
        return Task.FromResult(NextRegionView);
    }

    public Task SetRegionAsync(string? region)
    {
        _calls.Add("SetRegion");
        RegionSetCalls.Add(region);
        Throw();
        return Task.CompletedTask;
    }

    /// The seed-rotation roster fixture (default: a confirmable single-inheritor
    /// roster — the ordinary case).
    public FfiSeedRotationConfirmView NextSeedRotateRoster { get; set; } = new(
        new[] { MakeSeedRotationInheritor() }, true, null);
    /// The rotation-ceremony verdict fixture (default: a clean success).
    public FfiSeedRotationResult NextSeedRotationResult { get; set; } = new(
        true, new LocalizedText("admin.nest_page.rotate_seed_done", new()));
    public int SeedRotateRosterCalls { get; private set; }
    public int RotateDeploymentSeedCalls { get; private set; }

    public Task<FfiSeedRotationConfirmView> SeedRotateRosterAsync()
    {
        _calls.Add("SeedRotateRoster");
        SeedRotateRosterCalls++;
        Throw();
        return Task.FromResult(NextSeedRotateRoster);
    }

    public Task<FfiSeedRotationResult> RotateDeploymentSeedAsync()
    {
        _calls.Add("RotateDeploymentSeed");
        RotateDeploymentSeedCalls++;
        Throw();
        return Task.FromResult(NextSeedRotationResult);
    }

    // ── Post-auth passes over the session's one connection ──────────────
    /// What <see cref="SelfHealDeploymentSeedCustodyAsync"/> answers; null = it
    /// throws (a custody the leg could not confirm).
    public FfiDeploymentSeedSelfHeal? NextDeploymentSeedSelfHeal { get; set; } =
        new FfiDeploymentSeedSelfHeal.AlreadyCustodied();
    /// Completes <see cref="RunCriticalAlertSweepLoopAsync"/> — the loop otherwise
    /// runs until the test sets it, like the real one until identity teardown.
    public TaskCompletionSource CriticalAlertSweepLoopEnds { get; } = new();

    public Task<FfiDeploymentSeedSelfHeal> SelfHealDeploymentSeedCustodyAsync()
    {
        _calls.Add("SelfHealDeploymentSeedCustody");
        return NextDeploymentSeedSelfHeal is { } outcome
            ? Task.FromResult(outcome)
            : throw new InvalidOperationException("self-heal failed");
    }

    public Task RunCriticalAlertSweepAsync()
    {
        _calls.Add("RunCriticalAlertSweep");
        return Task.CompletedTask;
    }

    public Task RunCriticalAlertSweepLoopAsync()
    {
        _calls.Add("RunCriticalAlertSweepLoop");
        return CriticalAlertSweepLoopEnds.Task;
    }

    public Task ApplyPostClaimServingEnablementAsync(
        string nodeUrl, bool email, bool caldav, bool carddav, bool webdav)
    {
        _calls.Add($"ApplyPostClaimServingEnablement({nodeUrl},{email},{caldav},{carddav},{webdav})");
        return Task.CompletedTask;
    }

    public Task RecoveryRegisterDeferredKitAsync(string kitHex)
    {
        _calls.Add($"RecoveryRegisterDeferredKit({kitHex})");
        return Task.CompletedTask;
    }

    public Task<uniffi.fauna_client_pair.LinkedNestsMachine> BuildLinkedNestsMachineWithTrustAsync()
    {
        _calls.Add("BuildLinkedNestsMachineWithTrust");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a LinkedNestsMachine (needs a live FfiNestClient).");
    }

    /// The sign-in key set fixture (default: one signing key, nothing retired).
    public FfiIssuerKeyView NextIssuerKeyView { get; set; } = new(
        "kid-1", new[] { new FfiIssuerKeyRow("kid-1", true, null, null) }, 900, false);
    /// The ordinary/forced rotation verdict fixture (default: a clean success).
    public LocalizedText NextOauthVerdict { get; set; } =
        new("admin.nest_page.oauth_rotate_done", new() { ["kid"] = "kid-2" });
    public int AdminIssuerKeyStatusCalls { get; private set; }
    public int AdminRotateIssuerKeyCalls { get; private set; }
    /// Every <see cref="AdminForceRotateIssuerAsync"/> arg, in call order.
    public List<FfiIssuerForcedArm> ForceRotateCalls { get; } = new();
    /// Set to make ONLY <see cref="AdminIssuerKeyStatusAsync"/> throw, without
    /// affecting the shared <see cref="NextError"/> guard the rest of
    /// <c>LoadAsync</c>'s sequence already ran through by the time this call
    /// happens.
    public bool AdminIssuerKeyStatusThrows { get; set; }

    public Task<FfiIssuerKeyView> AdminIssuerKeyStatusAsync()
    {
        _calls.Add("AdminIssuerKeyStatus");
        AdminIssuerKeyStatusCalls++;
        if (AdminIssuerKeyStatusThrows) throw new InvalidOperationException("issuer key status failed");
        Throw();
        return Task.FromResult(NextIssuerKeyView);
    }

    public Task<LocalizedText> AdminRotateIssuerKeyAsync()
    {
        _calls.Add("AdminRotateIssuerKey");
        AdminRotateIssuerKeyCalls++;
        Throw();
        return Task.FromResult(NextOauthVerdict);
    }

    public Task<LocalizedText> AdminForceRotateIssuerAsync(FfiIssuerForcedArm arm)
    {
        _calls.Add("AdminForceRotateIssuer");
        ForceRotateCalls.Add(arm);
        Throw();
        return Task.FromResult(NextOauthVerdict);
    }

    /// The <c>fauna.admin.logs</c> nest-ring fixture (the admin Logs page source).
    public IReadOnlyList<LogEntry> NextAdminLogs { get; set; } = new List<LogEntry>();

    public Task<IReadOnlyList<LogEntry>> AdminLogsAsync()
    {
        _calls.Add("AdminLogs");
        Throw();
        return Task.FromResult(NextAdminLogs);
    }

    /// The <c>fauna.admin.custody_hosting.list</c> fixture (the
    /// admin-custody-hosting page source).
    public FfiAdminHostingRow[] NextAdminCustodyHostingRows { get; set; } = Array.Empty<FfiAdminHostingRow>();

    public Task<FfiAdminHostingRow[]> AdminCustodyHostingListAsync()
    {
        _calls.Add("AdminCustodyHostingList");
        Throw();
        return Task.FromResult(NextAdminCustodyHostingRows);
    }

    /// The <c>fauna.admin.custody_hosting.remove</c> fixture.
    public FfiAdminHostingRemoveReply NextAdminCustodyHostingRemoveReply { get; set; } = new(removed: true, storeDropped: false);
    public (string HostActorId, byte[] GrantId)? LastAdminCustodyHostingRemove { get; private set; }

    public Task<FfiAdminHostingRemoveReply> AdminCustodyHostingRemoveAsync(string hostActorId, byte[] grantId)
    {
        _calls.Add("AdminCustodyHostingRemove");
        LastAdminCustodyHostingRemove = (hostActorId, grantId);
        Throw();
        return Task.FromResult(NextAdminCustodyHostingRemoveReply);
    }


    // ── fauna.spam.* ────────────────────────────────────────────────────

    /// The <c>fauna.spam.get_preferences</c> fixture (per-mille u16).
    public FfiSpamPreferences NextSpamPreferences { get; set; } = MakeSpamPreferences();
    /// Last <c>set_preferences</c> args, for asserting the per-mille mapping.
    public (ushort? Spam, ushort? Phishing)? LastSpamSet { get; private set; }

    public Task<FfiSpamPreferences> SpamGetPreferencesAsync()
    {
        _calls.Add("SpamGetPreferences");
        Throw();
        return Task.FromResult(NextSpamPreferences);
    }

    public Task<FfiSpamPreferences> SpamSetPreferencesAsync(
        ushort? spamThreshold, ushort? phishingThreshold)
    {
        _calls.Add("SpamSetPreferences");
        LastSpamSet = (spamThreshold, phishingThreshold);
        Throw();
        // Echo the resulting full preferences (the nest reply echoes them).
        return Task.FromResult(MakeSpamPreferences(
            spamThreshold ?? NextSpamPreferences.spamThreshold,
            phishingThreshold ?? NextSpamPreferences.phishingThreshold));
    }

    /// The <c>fauna.moderation.actions</c> queue fixture (empty = nothing flagged).
    public IReadOnlyList<FfiObligationAction> NextModerationActions { get; set; } = new List<FfiObligationAction>();
    /// Last <c>fauna.moderation.train</c> args, for asserting the "ham" correction.
    public (string ContentId, string Verdict)? LastTrain { get; private set; }

    public Task<IReadOnlyList<FfiObligationAction>> ModerationActionsAsync()
    {
        _calls.Add("ModerationActions");
        Throw();
        return Task.FromResult(NextModerationActions);
    }

    public Task ModerationTrainAsync(string contentId, string verdict)
    {
        _calls.Add("ModerationTrain");
        LastTrain = (contentId, verdict);
        Throw();
        return Task.CompletedTask;
    }

    /// The <c>fauna.moderation.report_share.status</c> fixture (opt-in + published list).
    public FfiReportShareStatus NextReportShareStatus { get; set; } = new(false, Array.Empty<FfiReportShareEntry>());
    /// Last <c>fauna.moderation.report_share.set</c> arg, for asserting the toggle round-trip.
    public bool? LastReportShareSet { get; private set; }

    public Task<bool> ModerationReportShareSetAsync(bool share)
    {
        _calls.Add("ModerationReportShareSet");
        LastReportShareSet = share;
        Throw();
        NextReportShareStatus = NextReportShareStatus with { share = share };
        return Task.FromResult(share);
    }

    public Task<FfiReportShareStatus> ModerationReportShareStatusAsync()
    {
        _calls.Add("ModerationReportShareStatus");
        Throw();
        return Task.FromResult(NextReportShareStatus);
    }

    /// Last <c>fauna.moderation.legal_takedown</c> args, for asserting the
    /// admin console dispatches exactly what the operator armed.
    public (string ContentId, bool Conversation, string LegalReference, bool Restore)?
        LastLegalTakedown { get; private set; }

    public Task<string> ModerationLegalTakedownAsync(
        string contentId, bool conversation, string legalReference, bool restore)
    {
        _calls.Add("ModerationLegalTakedown");
        LastLegalTakedown = (contentId, conversation, legalReference, restore);
        Throw();
        return Task.FromResult(restore ? "restored" : "taken_down");
    }

    /// The <c>fauna.bridges.get_spam_threshold_override</c> fixture.
    public uint? NextSpamThresholdOverride { get; set; }
    /// Last <c>fauna.bridges.set_spam_threshold_override</c> arg, for asserting the round-trip.
    public uint? LastSpamThresholdOverrideSet { get; private set; }

    public Task<uint?> SpamThresholdOverrideGetAsync()
    {
        _calls.Add("SpamThresholdOverrideGet");
        Throw();
        return Task.FromResult(NextSpamThresholdOverride);
    }

    public Task<uint?> SpamThresholdOverrideSetAsync(uint? value)
    {
        _calls.Add("SpamThresholdOverrideSet");
        LastSpamThresholdOverrideSet = value;
        Throw();
        NextSpamThresholdOverride = value;
        return Task.FromResult(NextSpamThresholdOverride);
    }

    // ── Layer-B signal sharing (engagement-cues.md § Layer B) ───────────
    // Rides the (mock) shared FfiFeedManager, not FfiModerationClient — but the
    // mock has no live manager to fake (BuildFeedManagerAsync below is
    // NotSupported), so these fixtures stand in directly, mirroring the
    // ModerationReportShare* fixtures immediately above.

    /// The <c>fauna.moderation.signal_share.status</c> fixture (opt-in + published list).
    public FfiReportShareStatus NextSignalShareStatus { get; set; } = new(false, Array.Empty<FfiReportShareEntry>());
    /// Last <c>fauna.moderation.signal_share.set</c> arg, for asserting the toggle round-trip.
    public bool? LastSignalSharingSet { get; private set; }

    public Task<FfiReportShareStatus> SignalShareStatusAsync()
    {
        _calls.Add("SignalShareStatus");
        Throw();
        return Task.FromResult(NextSignalShareStatus);
    }

    public Task<FfiReportShareStatus> SetSignalSharingAsync(bool share)
    {
        _calls.Add("SetSignalSharing");
        LastSignalSharingSet = share;
        Throw();
        NextSignalShareStatus = NextSignalShareStatus with { share = share };
        return Task.FromResult(NextSignalShareStatus);
    }

    // ── fauna.family.* (Family page) ────────────────────────────────────

    /// <summary>The <c>fauna.family.status</c> fixture — no relationship by default.</summary>
    public FfiFamilyStatus NextFamilyStatus { get; set; } = new(
        null, null, Array.Empty<FfiFamilyWardInfo>(), Array.Empty<FfiFamilyIncomingTransfer>(),
        usageTodayMinutes: null, contactRequests: Array.Empty<FfiFamilyContactRequest>(),
        feedRequests: Array.Empty<FfiFamilyFeedRequest>(), ageBand: null, supervision: null);
    /// <summary>The <c>fauna.family.approvals.list</c> fixture.</summary>
    public IReadOnlyList<FfiFamilyApprovalEntry> NextFamilyApprovals { get; set; } = Array.Empty<FfiFamilyApprovalEntry>();
    /// <summary>Last <c>fauna.family.policy.update</c> args.</summary>
    public (byte[] SupervisedActorId, FfiReachPolicy Policy)? LastFamilyPolicyUpdate { get; private set; }
    /// <summary>Last <c>fauna.family.approvals.decide</c> args.</summary>
    public (byte[] SupervisedActorId, string Kind, byte[] PeerActorId, byte[] MessageId, string BridgeId, string Operation, string Target, string PeerAddress, bool Approve)? LastFamilyApprovalsDecide { get; private set; }
    /// <summary>Last <c>fauna.family.contact.add</c> args.</summary>
    public (byte[] SupervisedActorId, byte[] PeerActorId)? LastFamilyContactAdd { get; private set; }
    /// <summary>Last <c>fauna.family.graduate</c> arg.</summary>
    public byte[]? LastFamilyGraduate { get; private set; }
    /// <summary>Last <c>fauna.family.transfer</c> args.</summary>
    public (byte[] SupervisedActorId, byte[] NewGuardianActorId)? LastFamilyTransfer { get; private set; }

    public Task<FfiFamilyStatus> FamilyStatusAsync()
    {
        _calls.Add("FamilyStatus");
        Throw();
        return Task.FromResult(NextFamilyStatus);
    }

    /// The `supervision_snapshot` restore-at-launch fixture — defaults to
    /// "nothing to enforce" (`null`), the same posture every unresolvable-hit
    /// fixture in this file takes.
    public FfiSupervisionSnapshot? NextSupervisionSnapshot { get; set; }
    /// Last `supervision_snapshot` actor id hex.
    public string? LastSupervisionSnapshotActorIdHex { get; private set; }

    public FfiSupervisionSnapshot? SupervisionSnapshot(string actorId)
    {
        _calls.Add("SupervisionSnapshot");
        LastSupervisionSnapshotActorIdHex = actorId;
        return NextSupervisionSnapshot;
    }

    public Task FamilyPolicyUpdateAsync(byte[] supervisedActorId, FfiReachPolicy policy)
    {
        _calls.Add("FamilyPolicyUpdate");
        LastFamilyPolicyUpdate = (supervisedActorId, policy);
        Throw();
        return Task.CompletedTask;
    }

    /// <summary>Last <c>fauna.family.notify_report</c> args; call count so a
    /// drained-then-no-op second tick is assertable.</summary>
    public (FfiFamilyContentNotice[] Entries, int OffsetMinutes)? LastFamilyNotifyReport { get; private set; }
    public int FamilyNotifyReportCallCount { get; private set; }

    public Task FamilyNotifyReportAsync(FfiFamilyContentNotice[] entries, int utcOffsetMinutes)
    {
        _calls.Add("FamilyNotifyReport");
        FamilyNotifyReportCallCount++;
        LastFamilyNotifyReport = (entries, utcOffsetMinutes);
        Throw();
        return Task.CompletedTask;
    }

    /// <summary>The <c>fauna.family.usage_report</c> reply fixture.</summary>
    public FfiFamilyUsageReport NextFamilyUsageReport { get; set; } = FfiFamilyUsageReportFixture.Make();
    /// <summary>Last <c>fauna.family.usage_report</c> args; call count so a
    /// drained-then-no-op second tick is assertable (mirrors <see
    /// cref="FamilyNotifyReportCallCount"/>).</summary>
    public (uint Minutes, int OffsetMinutes)? LastFamilyUsageReport { get; private set; }
    public int FamilyUsageReportCallCount { get; private set; }

    public Task<FfiFamilyUsageReport> FamilyUsageReportAsync(uint minutes, int utcOffsetMinutes)
    {
        _calls.Add("FamilyUsageReport");
        FamilyUsageReportCallCount++;
        LastFamilyUsageReport = (minutes, utcOffsetMinutes);
        Throw();
        return Task.FromResult(NextFamilyUsageReport);
    }

    public Task<IReadOnlyList<FfiFamilyApprovalEntry>> FamilyApprovalsListAsync()
    {
        _calls.Add("FamilyApprovalsList");
        Throw();
        return Task.FromResult(NextFamilyApprovals);
    }

    public Task FamilyApprovalsDecideAsync(byte[] supervisedActorId, string kind, byte[] peerActorId, byte[] messageId, string bridgeId, string operation, string target, string peerAddress, bool approve)
    {
        _calls.Add("FamilyApprovalsDecide");
        LastFamilyApprovalsDecide = (supervisedActorId, kind, peerActorId, messageId, bridgeId, operation, target, peerAddress, approve);
        Throw();
        return Task.CompletedTask;
    }

    public Task FamilyContactAddAsync(byte[] supervisedActorId, byte[] peerActorId)
    {
        _calls.Add("FamilyContactAdd");
        LastFamilyContactAdd = (supervisedActorId, peerActorId);
        Throw();
        return Task.CompletedTask;
    }

    public Task FamilyGraduateAsync(byte[] supervisedActorId)
    {
        _calls.Add("FamilyGraduate");
        LastFamilyGraduate = supervisedActorId;
        Throw();
        return Task.CompletedTask;
    }

    public Task FamilyTransferAsync(byte[] supervisedActorId, byte[] newGuardianActorId)
    {
        _calls.Add("FamilyTransfer");
        LastFamilyTransfer = (supervisedActorId, newGuardianActorId);
        Throw();
        return Task.CompletedTask;
    }

    /// <summary>Last <c>fauna.family.transfer.accept</c> arg.</summary>
    public byte[]? LastFamilyTransferAccept { get; private set; }
    /// <summary>Last <c>fauna.family.transfer.decline</c> arg.</summary>
    public byte[]? LastFamilyTransferDecline { get; private set; }
    /// <summary>Last <c>fauna.family.transfer.cancel</c> arg.</summary>
    public byte[]? LastFamilyTransferCancel { get; private set; }

    public Task FamilyTransferAcceptAsync(byte[] supervisedActorId)
    {
        _calls.Add("FamilyTransferAccept");
        LastFamilyTransferAccept = supervisedActorId;
        Throw();
        return Task.CompletedTask;
    }

    public Task FamilyTransferDeclineAsync(byte[] supervisedActorId)
    {
        _calls.Add("FamilyTransferDecline");
        LastFamilyTransferDecline = supervisedActorId;
        Throw();
        return Task.CompletedTask;
    }

    public Task FamilyTransferCancelAsync(byte[] supervisedActorId)
    {
        _calls.Add("FamilyTransferCancel");
        LastFamilyTransferCancel = supervisedActorId;
        Throw();
        return Task.CompletedTask;
    }

    /// <summary>Last <c>fauna.family.device.mark</c> args.</summary>
    public (byte[] SupervisedActorId, string DeviceId, bool Marked)? LastFamilyDeviceMark { get; private set; }

    public Task FamilyDeviceMarkAsync(byte[] supervisedActorId, string deviceId, bool marked)
    {
        _calls.Add("FamilyDeviceMark");
        LastFamilyDeviceMark = (supervisedActorId, deviceId, marked);
        Throw();
        return Task.CompletedTask;
    }

    /// The <c>fauna.posts.get</c> body-text fixture keyed by content_id (a moderation
    /// server-row spam train fetches the post body via this). Missing key ⇒ null.
    public Dictionary<string, string> NextPostBody { get; set; } = new();

    public Task<string?> PostBodyTextAsync(string contentId)
    {
        _calls.Add("PostBodyText");
        Throw();
        return Task.FromResult(NextPostBody.TryGetValue(contentId, out var b) ? b : null);
    }

    // ── fauna.folders.* ───────────────────────────────────────────────

    /// The <c>fauna.folders.list</c> fixture (the Media page's collection list).
    public IReadOnlyList<FfiFolder> NextFolders { get; set; } = new List<FfiFolder>();
    /// Last <c>fauna.folders.create</c> args (name + mode).
    public string? LastFolderCreate { get; private set; }

    public Task<IReadOnlyList<FfiFolder>> FoldersListAsync()
    {
        _calls.Add("FoldersList");
        Throw();
        return Task.FromResult(NextFolders);
    }

    public Task<FfiFolder> FoldersCreateAsync(string name)
    {
        _calls.Add("FoldersCreate");
        LastFolderCreate = name;
        Throw();
        return Task.FromResult(MakeFolder(name: name));
    }

    /// Count of <see cref="RunSealBackfillSweepAsync"/> calls.
    public int RunSealBackfillSweepCalls { get; private set; }
    /// The <c>run_seal_backfill_sweep</c> fixture — an all-quiet/converged report
    /// by default (no fields pass, no tag failures, nothing swept).
    public FfiSealBackfillSweepReport NextSealBackfillSweepReport { get; set; } =
        new(fields: null, fieldsError: null, tags: new(0, 0, 0), setsSwept: 0, memberSetsSkipped: 0, setFailures: 0, rosterError: null);

    public Task<FfiSealBackfillSweepReport> RunSealBackfillSweepAsync()
    {
        _calls.Add("RunSealBackfillSweep");
        RunSealBackfillSweepCalls++;
        Throw();
        return Task.FromResult(NextSealBackfillSweepReport);
    }

    // ── Cross-user sharing (owner side) — folders.md § Sharing ─────────

    /// The <c>fauna.folders.members.list_actors</c> fixture.
    public IReadOnlyList<FfiFolderActorMember> NextFolderActors { get; set; } = new List<FfiFolderActorMember>();
    /// Last <c>members.list_actors</c> set name.
    public string? LastFolderActorsQuery { get; private set; }
    /// The <c>folders_share</c> fixture.
    public FfiShareOutcome NextShareOutcome { get; set; } = new(new byte[32], 0);
    /// Last <c>folders_share</c> args (name, member id hex, member nest url).
    public (string Name, string MemberIdHex, string? MemberNestUrl)? LastFolderShare { get; private set; }

    /// The multi-writer Phase 1 grant the last share was invited with (`"writer"`,
    /// `"reader"`, or null for the read-only default).
    public string? LastFolderShareAccess { get; private set; }
    /// The <c>folders_remove_member</c> fixture.
    public FfiRemoveOutcome NextRemoveOutcome { get; set; } = new(null, true, true);
    /// Last <c>folders_remove_member</c> args (name, channel id hex, member id hex).
    public (string Name, string ChannelIdHex, string MemberIdHex)? LastFolderRemoveMember { get; private set; }

    public Task<IReadOnlyList<FfiFolderActorMember>> FoldersMembersListActorsAsync(string name)
    {
        _calls.Add("FoldersMembersListActors");
        LastFolderActorsQuery = name;
        Throw();
        return Task.FromResult(NextFolderActors);
    }

    public (string name, string actorIdHex, string access, long? byteCap)? LastFolderMemberAccessSet { get; private set; }

    public Task FoldersSetMemberAccessAsync(string name, string actorIdHex, string access, long? byteCap)
    {
        _calls.Add("FoldersSetMemberAccess");
        LastFolderMemberAccessSet = (name, actorIdHex, access, byteCap);
        Throw();
        return Task.CompletedTask;
    }

    /// The <c>fauna.folders.devices</c> fixture.
    public IReadOnlyList<FfiFolderDevice> NextFolderDevices { get; set; } = new List<FfiFolderDevice>();
    /// Last <c>folders.devices</c> set name.
    public string? LastFolderDevicesQuery { get; private set; }

    public Task<IReadOnlyList<FfiFolderDevice>> FoldersDevicesAsync(string name)
    {
        _calls.Add("FoldersDevices");
        LastFolderDevicesQuery = name;
        Throw();
        return Task.FromResult(NextFolderDevices);
    }

    /// <summary>The set's DEVICE roster, feeding the place editor. Seats arrive with
    /// the projection already applied, so a test seeds the flag triple it wants
    /// rendered rather than a role to be resolved.</summary>
    public IReadOnlyList<FfiFolderMember> NextFolderMembers { get; set; } = new List<FfiFolderMember>();

    public string? LastFolderMembersQuery { get; private set; }

    public Task<IReadOnlyList<FfiFolderMember>> FoldersPlaceRowsAsync(
        string name, IReadOnlyList<uniffi.fauna_devices_machine.DeviceSummary> devices)
    {
        _calls.Add("FoldersPlaceRows");
        LastFolderMembersQuery = name;
        Throw();
        return Task.FromResult(NextFolderMembers);
    }

    public Task<FfiShareOutcome> FoldersShareAsync(ConversationsSession session, string name, byte[] memberId, string? memberNestUrl, string? access = null)
    {
        _calls.Add("FoldersShare");
        LastFolderShare = (name, Convert.ToHexString(memberId), memberNestUrl);
        LastFolderShareAccess = access;
        Throw();
        return Task.FromResult(NextShareOutcome);
    }

    public Task<FfiRemoveOutcome> FoldersRemoveMemberAsync(ConversationsSession session, string name, byte[] channelId, byte[] memberId)
    {
        _calls.Add("FoldersRemoveMember");
        LastFolderRemoveMember = (name, Convert.ToHexString(channelId), Convert.ToHexString(memberId));
        Throw();
        return Task.FromResult(NextRemoveOutcome);
    }

    /// The <c>folders_serve_set</c> fixture (served-set count the blob now carries).
    public uint NextServeSetResult { get; set; }
    /// Last <c>folders_serve_set</c> args (name, mls group id hex, enable).
    public (string Name, string? MlsGroupIdHex, bool Enable)? LastFolderServeSet { get; private set; }

    public Task<uint> FoldersServeSetAsync(ConversationsSession session, string name, string? mlsGroupIdHex, bool enable)
    {
        _calls.Add("FoldersServeSet");
        LastFolderServeSet = (name, mlsGroupIdHex, enable);
        Throw();
        return Task.FromResult(NextServeSetResult);
    }

    /// Last <c>folders_paywall_set</c> args (name, mls group id hex, tier) — the
    /// web-type sibling of <see cref="LastFolderServeSet"/>.
    public (string Name, string? MlsGroupIdHex, string Tier)? LastFolderPaywallSet { get; private set; }

    public Task FoldersPaywallSetAsync(ConversationsSession session, string name, string? mlsGroupIdHex, string tier)
    {
        _calls.Add("FoldersPaywallSet");
        LastFolderPaywallSet = (name, mlsGroupIdHex, tier);
        Throw();
        return Task.CompletedTask;
    }

    /// The <c>folders_can_serve_webdav</c> fixture — defaults to <c>true</c> so
    /// existing tests that don't care about the gate see the toggle enabled.
    public bool NextCanServeWebdavResult { get; set; } = true;

    public Task<bool> FoldersCanServeWebdavAsync()
    {
        _calls.Add("FoldersCanServeWebdav");
        Throw();
        return Task.FromResult(NextCanServeWebdavResult);
    }

    // ── Cross-user sharing (recipient side) — folders.md § Sharing ─────

    /// Last <c>folders_leave</c> group-id hex arg (recipient-side voluntary leave).
    public string? LastFolderLeave { get; private set; }

    public Task FoldersLeaveAsync(ConversationsSession session, string groupIdHex)
    {
        _calls.Add("FoldersLeave");
        LastFolderLeave = groupIdHex;
        Throw();
        return Task.CompletedTask;
    }

    /// The <c>fauna.folders.pending_shares</c> fixture.
    public IReadOnlyList<FfiPendingShare> NextPendingShares { get; set; } = new List<FfiPendingShare>();
    /// Last <c>folders_accept_share</c> inbox-id arg.
    public long? LastFolderAcceptShare { get; private set; }
    /// Last <c>folders_decline_share</c> inbox-id arg.
    public long? LastFolderDeclineShare { get; private set; }

    public Task<IReadOnlyList<FfiPendingShare>> FoldersPendingSharesAsync()
    {
        _calls.Add("FoldersPendingShares");
        Throw();
        return Task.FromResult(NextPendingShares);
    }

    public Task FoldersAcceptShareAsync(ConversationsSession session, long inboxId)
    {
        _calls.Add("FoldersAcceptShare");
        LastFolderAcceptShare = inboxId;
        Throw();
        return Task.CompletedTask;
    }

    public Task FoldersDeclineShareAsync(long inboxId)
    {
        _calls.Add("FoldersDeclineShare");
        LastFolderDeclineShare = inboxId;
        Throw();
        return Task.CompletedTask;
    }

#if P2P_SHARE
    /// <summary>
    /// The offline-share panel is rendered directly off the page's own held
    /// <c>FfiCeremonySeat</c> (mirrors the wizard/devices machines above), and
    /// binding one needs a real <c>FfiNestClient</c>+libfauna_ffi round-trip;
    /// unsupported here.
    /// </summary>
    public Task<FfiCeremonySeat> BindOfflineShareSeatAsync()
    {
        _calls.Add("OfflineShareBindSeat");
        throw new NotSupportedException(
            "MockNestRpcClient cannot bind a live FfiCeremonySeat (needs a live FfiNestClient).");
    }

    /// Last <c>offline_share_initiate</c> peer-code-input arg.
    public string? LastOfflineShareInitiatePeerCode { get; private set; }
    /// The <c>offline_share_initiate</c> fixture result.
    public CeremonyStatus NextOfflineShareInitiateStatus { get; set; } = CeremonyStatus.OfferSent;

    public Task<CeremonyStatus> OfflineShareInitiateAsync(FfiCeremonySeat seat, string peerCodeInput)
    {
        _calls.Add("OfflineShareInitiate");
        LastOfflineShareInitiatePeerCode = peerCodeInput;
        Throw();
        return Task.FromResult(NextOfflineShareInitiateStatus);
    }

    /// Last <c>offline_share_consent</c> scope-id arg.
    public byte[]? LastOfflineShareConsentScopeId { get; private set; }
    /// The <c>offline_share_consent</c> fixture result.
    public CeremonyStatus NextOfflineShareConsentStatus { get; set; } = CeremonyStatus.Admitted;

    public Task<CeremonyStatus> OfflineShareConsentAsync(FfiCeremonySeat seat, byte[] scopeId)
    {
        _calls.Add("OfflineShareConsent");
        LastOfflineShareConsentScopeId = scopeId;
        Throw();
        return Task.FromResult(NextOfflineShareConsentStatus);
    }

    /// Last <c>offline_share_decline</c> scope-id arg.
    public byte[]? LastOfflineShareDeclineScopeId { get; private set; }

    public Task OfflineShareDeclineAsync(FfiCeremonySeat seat, byte[] scopeId)
    {
        _calls.Add("OfflineShareDecline");
        LastOfflineShareDeclineScopeId = scopeId;
        Throw();
        return Task.CompletedTask;
    }

    /// The <c>offline_share_load_group_shares</c> fixture.
    public FfiGroupShareViews NextGroupShareViews { get; set; } = MakeGroupShareViews();

    public Task<FfiGroupShareViews> OfflineShareLoadGroupSharesAsync(FfiCeremonySeat? seat = null)
    {
        _calls.Add("OfflineShareLoadGroupShares");
        Throw();
        return Task.FromResult(NextGroupShareViews);
    }
#endif

    // ── fauna.search.query ──────────────────────────────────────────────

    /// The <c>fauna.search.query</c> fixture (the Search page's result rows).
    public IReadOnlyList<FfiSearchResult> NextSearchResults { get; set; } = new List<FfiSearchResult>();
    /// Pagination test support: when >= 0, <c>SearchQueryAsync</c> synthesizes
    /// <c>min(SearchAvailable, limit)</c> rows (mirrors the nest clamping the
    /// reply to <c>limit</c>), exercising the limit-growing load-more flow.
    public int SearchAvailable { get; set; } = -1;
    /// Records each call's (offset, limit), so tests assert the grown page.
    public readonly List<(long Offset, long Limit)> SearchParams = new();

    public Task<IReadOnlyList<FfiSearchResult>> SearchQueryAsync(
        string query, string? contentType, long? limit, long? offset)
    {
        _calls.Add("SearchQuery");
        SearchParams.Add((offset ?? 0, limit ?? 0));
        Throw();
        if (SearchAvailable >= 0)
        {
            int n = (int)Math.Min(SearchAvailable, limit ?? 20);
            var rows = new List<FfiSearchResult>(n);
            for (int i = 0; i < n; i++)
                rows.Add(MakeSearchResult(contentId: $"r{i}", snippet: $"S{i}"));
            return Task.FromResult<IReadOnlyList<FfiSearchResult>>(rows);
        }
        return Task.FromResult(NextSearchResults);
    }

    // ── fauna.filesync.snapshot.* / fauna.sync.backup_status (Backups) ──

    public IReadOnlyList<FolderStatus> NextBackupStatus { get; set; } = new List<FolderStatus>();
    public IReadOnlyList<SnapshotInfo> NextSnapshots { get; set; } = new List<SnapshotInfo>();
    public SnapshotDetailInfo? NextSnapshotDetail { get; set; }

    /// Last <c>fauna.filesync.snapshot.list</c> folder argument.
    public string? LastListFolder { get; private set; }
    /// Last <c>fauna.filesync.snapshot.create_folder</c> args.
    public (string Folder, IReadOnlyList<string> Tags)? LastCreateFolder { get; private set; }
    /// Last <c>fauna.filesync.snapshot.delete</c> snapshot id.
    public ulong? LastDeletedSnapshot { get; private set; }
    /// Last <c>fauna.filesync.snapshot.prune</c> args.
    public (string Folder, uint? KeepLast, uint? KeepDaily, uint? KeepWeekly, uint? KeepMonthly)? LastPrune { get; private set; }

    public Task<IReadOnlyList<FolderStatus>> BackupStatusAsync()
    {
        _calls.Add("BackupStatus");
        Throw();
        return Task.FromResult(NextBackupStatus);
    }

    public Task<IReadOnlyList<SnapshotInfo>> SnapshotListAsync(string folder)
    {
        _calls.Add("SnapshotList");
        LastListFolder = folder;
        Throw();
        return Task.FromResult(NextSnapshots);
    }

    public Task<SnapshotDetailInfo> SnapshotGetAsync(ulong id)
    {
        _calls.Add("SnapshotGet");
        Throw();
        return Task.FromResult(NextSnapshotDetail
            ?? new SnapshotDetailInfo(id, "default", 0, 0, 0, new List<string>(), null, new List<SnapshotFileInfo>()));
    }

    public Task<SnapshotInfo> SnapshotCreateFolderAsync(string folder, IReadOnlyList<string> tags)
    {
        _calls.Add("SnapshotCreateFolder");
        LastCreateFolder = (folder, tags);
        Throw();
        return Task.FromResult(new SnapshotInfo(1, 0, 0, 0, tags.ToList(), null));
    }

    public Task SnapshotDeleteAsync(ulong id)
    {
        _calls.Add("SnapshotDelete");
        LastDeletedSnapshot = id;
        Throw();
        return Task.CompletedTask;
    }

    public Task SnapshotPruneAsync(string folder, uint? keepLast, uint? keepDaily, uint? keepWeekly, uint? keepMonthly)
    {
        _calls.Add("SnapshotPrune");
        LastPrune = (folder, keepLast, keepDaily, keepWeekly, keepMonthly);
        Throw();
        return Task.CompletedTask;
    }

    // ── Restore surface ─────────────────────────────────────────────────

    public IReadOnlyList<FfiRestoreHistoryRow> NextRestoreHistory { get; set; } = new List<FfiRestoreHistoryRow>();
    /// Forensic divergence rows keyed by snapshot id (empty/absent ⇒ no banner).
    public Dictionary<long, IReadOnlyList<FfiRestoreDivergenceRow>> NextRestoreDivergence { get; } = new();
    /// Set to make every <c>SnapshotListRestoreDivergenceAsync</c> throw (the
    /// per-row degrade path — history still loads).
    public string? DivergenceError { get; set; }
    public IReadOnlyList<FfiSnapshotSummary> NextMessageKindSnapshots { get; set; } = new List<FfiSnapshotSummary>();
    public FfiSnapshotRestoreReply NextRestoreReply { get; set; } = new(0, "mail", true, "");
    /// Last <c>snapshot_restore_message_kind</c> args (asserts the friction-bar pass-through).
    public (long SnapshotId, string ConfirmId)? LastRestoreCall { get; private set; }

    public Task<IReadOnlyList<FfiRestoreHistoryRow>> SnapshotListRestoreHistoryAsync(uint limit = 0)
    {
        _calls.Add("SnapshotListRestoreHistory");
        Throw();
        return Task.FromResult(NextRestoreHistory);
    }

    public Task<IReadOnlyList<FfiRestoreDivergenceRow>> SnapshotListRestoreDivergenceAsync(long snapshotId)
    {
        _calls.Add("SnapshotListRestoreDivergence");
        if (DivergenceError is not null) throw new InvalidOperationException(DivergenceError);
        Throw();
        return Task.FromResult(
            NextRestoreDivergence.TryGetValue(snapshotId, out var rows)
                ? rows
                : (IReadOnlyList<FfiRestoreDivergenceRow>)new List<FfiRestoreDivergenceRow>());
    }

    public Task<FfiSnapshotRestoreReply> SnapshotRestoreMessageKindAsync(long snapshotId, string confirmId)
    {
        _calls.Add("SnapshotRestoreMessageKind");
        LastRestoreCall = (snapshotId, confirmId);
        Throw();
        return Task.FromResult(NextRestoreReply);
    }

    /// Last <c>snapshot_delete_immediate</c> args (asserts the modal friction-bar pass-through).
    public (long SnapshotId, string ConfirmId, string Acknowledge)? LastImmediateDeleteCall { get; private set; }

    public Task SnapshotDeleteImmediateAsync(long snapshotId, string confirmId, string acknowledge)
    {
        _calls.Add("SnapshotDeleteImmediate");
        LastImmediateDeleteCall = (snapshotId, confirmId, acknowledge);
        Throw();
        return Task.CompletedTask;
    }

    public Task<IReadOnlyList<FfiSnapshotSummary>> MessageKindSnapshotListAsync()
    {
        _calls.Add("MessageKindSnapshotList");
        Throw();
        return Task.FromResult(NextMessageKindSnapshots);
    }

    /// A <c>FfiRestoreHistoryRow</c> fixture; <paramref name="sourceMemberId"/> null
    /// ⇒ the row renders "local snapshot".
    public static FfiRestoreHistoryRow MakeRestoreHistory(
        long snapshotId, string kindsRestored = "mail", byte[]? sourceMemberId = null,
        long id = 1, long completedAt = 1_700_000_000) =>
        new FfiRestoreHistoryRow(id, completedAt, snapshotId, kindsRestored, sourceMemberId);

    /// A <c>FfiRestoreDivergenceRow</c> fixture (forensic modseq divergence).
    public static FfiRestoreDivergenceRow MakeRestoreDivergence(
        long snapshotId, string protocol = "imap", string collection = "INBOX",
        string? muaId = null, long clientModseq = 99, long serverModseq = 1, long lostEventCount = 98,
        long id = 1, long observedAt = 1_700_000_500) =>
        new FfiRestoreDivergenceRow(
            id, snapshotId, observedAt, protocol, collection, muaId,
            clientModseq, serverModseq, lostEventCount);

    /// A <c>FfiSnapshotSummary</c> fixture for the restore picker.
    public static FfiSnapshotSummary MakeSnapshotSummary(
        long id, string? messageKind = "mail", long createdAt = 1_700_000_000) =>
        new FfiSnapshotSummary(id, createdAt, messageKind, 0, 0, null);

    // ── Backup destinations (management) ────────────────────────────────

    /// The list each backup-destination call returns (the seam returns the
    /// freshly-persisted list every time — tests set this to model the post-op state).
    public IReadOnlyList<FfiBackupDestinationView> NextDestinations { get; set; }
        = new List<FfiBackupDestinationView>();
    /// Set to make the next add/edit/remove throw (e.g. an
    /// <c>FfiException.General</c> carrying the different-nest sentinel).
    public Exception? NextDestinationException { get; set; }
    public (string Url, string Name)? LastDestinationAdd { get; private set; }
    public (string Id, string Url, string Name)? LastDestinationEdit { get; private set; }
    public string? LastDestinationRemove { get; private set; }

    public Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationsListAsync()
    {
        _calls.Add("BackupDestinationsList");
        Throw();
        return Task.FromResult(NextDestinations);
    }

    public Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationAddAsync(string url, string name)
    {
        _calls.Add("BackupDestinationAdd");
        LastDestinationAdd = (url, name);
        if (NextDestinationException is not null) throw NextDestinationException;
        Throw();
        return Task.FromResult(NextDestinations);
    }

    public Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationEditAsync(string id, string url, string name)
    {
        _calls.Add("BackupDestinationEdit");
        LastDestinationEdit = (id, url, name);
        if (NextDestinationException is not null) throw NextDestinationException;
        Throw();
        return Task.FromResult(NextDestinations);
    }

    public Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationRemoveAsync(string id)
    {
        _calls.Add("BackupDestinationRemove");
        LastDestinationRemove = id;
        if (NextDestinationException is not null) throw NextDestinationException;
        Throw();
        return Task.FromResult(NextDestinations);
    }

    public string? LastDestinationKeep { get; private set; }

    public Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationKeepAsync(string id)
    {
        _calls.Add("BackupDestinationKeep");
        LastDestinationKeep = id;
        if (NextDestinationException is not null) throw NextDestinationException;
        Throw();
        return Task.FromResult(NextDestinations);
    }

    public (string CustodianDeviceId, string Name, ulong? CapacityCapBytes)? LastCustodianEnroll { get; private set; }

    public Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationEnrollCustodianAsync(
        string custodianDeviceId, string name, ulong? capacityCapBytes)
    {
        _calls.Add("BackupDestinationEnrollCustodian");
        LastCustodianEnroll = (custodianDeviceId, name, capacityCapBytes);
        if (NextDestinationException is not null) throw NextDestinationException;
        Throw();
        return Task.FromResult(NextDestinations);
    }

    /// The live per-destination status each read returns (tests set this to model the
    /// coordinator's computed backlog / last-upload-time).
    public IReadOnlyList<FfiBackupDestinationStatus> NextDestinationStatuses { get; set; }
        = new List<FfiBackupDestinationStatus>();

    public Task<IReadOnlyList<FfiBackupDestinationStatus>> BackupDestinationStatusAsync()
    {
        _calls.Add("BackupDestinationStatus");
        Throw();
        return Task.FromResult(NextDestinationStatuses);
    }

    /// The places list every folder-destination call returns (the seam returns the
    /// folder's freshly re-read places every time — tests set this to model the
    /// post-op state).
    public IReadOnlyList<FfiFolderDestinationPlace> NextFolderDestinationPlaces { get; set; }
        = new List<FfiFolderDestinationPlace>();
    public (long FolderId, string DestinationId)? LastFolderDestinationAttach { get; private set; }
    public (long FolderId, string DestinationId, string FolderSet)? LastFolderDestinationDetach { get; private set; }

    public Task<IReadOnlyList<FfiFolderDestinationPlace>> FolderDestinationsListAsync(long folderId)
    {
        _calls.Add("FolderDestinationsList");
        Throw();
        return Task.FromResult(NextFolderDestinationPlaces);
    }

    public Task<IReadOnlyList<FfiFolderDestinationPlace>> FolderDestinationAttachAsync(long folderId, string destinationId)
    {
        _calls.Add("FolderDestinationAttach");
        LastFolderDestinationAttach = (folderId, destinationId);
        Throw();
        return Task.FromResult(NextFolderDestinationPlaces);
    }

    public Task<IReadOnlyList<FfiFolderDestinationPlace>> FolderDestinationDetachAsync(long folderId, string destinationId, string folderSet)
    {
        _calls.Add("FolderDestinationDetach");
        LastFolderDestinationDetach = (folderId, destinationId, folderSet);
        Throw();
        return Task.FromResult(NextFolderDestinationPlaces);
    }

    /// The roster every `MemberReviewsListAsync` returns (tests set this to model
    /// the owner's open review items).
    public IReadOnlyList<FfiMemberReview> NextMemberReviews { get; set; }
        = new List<FfiMemberReview>();
    /// `MemberReviewKeepAsync`'s return (whether anything was actually open).
    public bool NextMemberReviewKeepResult { get; set; } = true;
    public byte[]? LastMemberReviewKeep { get; private set; }

    public Task<IReadOnlyList<FfiMemberReview>> MemberReviewsListAsync()
    {
        _calls.Add("MemberReviewsList");
        Throw();
        return Task.FromResult(NextMemberReviews);
    }

    public Task<bool> MemberReviewKeepAsync(byte[] person)
    {
        _calls.Add("MemberReviewKeep");
        LastMemberReviewKeep = person;
        Throw();
        return Task.FromResult(NextMemberReviewKeepResult);
    }

    /// The ids every `FilterMarksListAsync` returns (the owner's open
    /// email-filter review marks).
    public IReadOnlyList<long> NextFilterMarks { get; set; } = new List<long>();
    public long? LastFilterMarkKeep { get; private set; }
    public long? LastFilterMarkRemoved { get; private set; }

    public Task<IReadOnlyList<long>> FilterMarksListAsync()
    {
        _calls.Add("FilterMarksList");
        Throw();
        return Task.FromResult(NextFilterMarks);
    }

    public Task<bool> FilterMarkKeepAsync(long filterId)
    {
        _calls.Add("FilterMarkKeep");
        LastFilterMarkKeep = filterId;
        Throw();
        return Task.FromResult(true);
    }

    public Task<bool> FilterMarkRemovedAsync(long filterId)
    {
        _calls.Add("FilterMarkRemoved");
        LastFilterMarkRemoved = filterId;
        Throw();
        return Task.FromResult(true);
    }

    /// `MemberReviewRemoveAsync`'s return — defaults to a complete eviction of
    /// nobody (evicted/failed/unreachable all empty), which still EARNS
    /// `Removed` (an eviction that frees nobody earns it too — the ordinary
    /// case on a deferred backlog whose person already left every group).
    public CrossGroupEviction NextMemberReviewRemoveResult { get; set; }
        = new(Array.Empty<string>(), Array.Empty<EvictionFailure>(), Array.Empty<UnreachableSeat>());
    public byte[]? LastMemberReviewRemove { get; private set; }

    public Task<CrossGroupEviction> MemberReviewRemoveAsync(ConversationsManager manager, byte[] person)
    {
        _calls.Add("MemberReviewRemove");
        LastMemberReviewRemove = person;
        Throw();
        return Task.FromResult(NextMemberReviewRemoveResult);
    }

    /// The audit rows the next `BackupAuditRunPassAsync` returns (tests set this to
    /// model the audit pass's per-destination `alert_reason`).
    public IReadOnlyList<FfiDestinationAuditRow> NextAuditRows { get; set; }
        = new List<FfiDestinationAuditRow>();

    public string? LastAuditStatePath { get; private set; }
    public string? LastAuditSyncStateDir { get; private set; }

    public Task<IReadOnlyList<FfiDestinationAuditRow>> BackupAuditRunPassAsync(
        string statePath, string? syncStateDir)
    {
        _calls.Add("BackupAuditRunPass");
        LastAuditStatePath = statePath;
        LastAuditSyncStateDir = syncStateDir;
        Throw();
        return Task.FromResult(NextAuditRows);
    }

    /// Whether the next `BackupAuditObserve` call reports the high-water advanced.
    public bool NextAuditObserveAdvanced { get; set; }
    public (string StatePath, long LastActivityMs)? LastAuditObserve { get; private set; }

    public bool BackupAuditObserve(string statePath, long lastActivityMs)
    {
        _calls.Add("BackupAuditObserve");
        LastAuditObserve = (statePath, lastActivityMs);
        return NextAuditObserveAdvanced;
    }

    /// The decrypted bytes the shared-walk single-file download returns
    /// (`snapshot-file-download-button`).
    public byte[] NextSnapshotFileBytes { get; set; } = Array.Empty<byte>();
    public (string DeviceId, ulong SnapshotId, string Path)? LastSnapshotFileDownload { get; private set; }

    public Task<byte[]> DownloadSnapshotFileBytesAsync(string deviceId, ulong snapshotId, string path)
    {
        _calls.Add("DownloadSnapshotFileBytes");
        LastSnapshotFileDownload = (deviceId, snapshotId, path);
        Throw();
        return Task.FromResult(NextSnapshotFileBytes);
    }

    /// A <c>FfiBackupDestinationView</c> fixture (UI projection of a destination).
    /// The per-kind tail defaults to a plain nest row — <c>kind: "nest"</c> is what
    /// the serde default gives every pre-existing destination, and the two
    /// custodian-only fields are <c>null</c> there — so a test builds a custodian
    /// row by naming them (<c>kind: "client-device"</c>, <c>custodianDeviceId: …</c>).
    public static FfiBackupDestinationView MakeDestination(
        string id, string url, string? displayName,
        string kind = "nest", string? custodianDeviceId = null,
        ulong? capacityCapBytes = null, bool unattested = false) =>
        new FfiBackupDestinationView(
            id, url, displayName, kind, custodianDeviceId, capacityCapBytes,
            unattested: unattested);

    // ── Muted keywords ────────────────────────────────────────────────────
    /// The list each muted-keywords call returns (the seam returns the
    /// freshly-persisted list every time — tests set this to model the post-op state).
    /// A returned record is always <c>loaded</c>: the real seam mints that bit only
    /// on a completed round trip, and a failure is modelled with <c>Throw()</c>.
    public IReadOnlyList<string> NextMutedKeywords { get; set; } = new List<string>();
    public IReadOnlyList<uniffi.fauna_core.MutedKeyword>? LastMutedKeywordsSet { get; private set; }

    /// The queued terms as the shared record carries them: each muted at the
    /// default weight, the full penalty (−1000,
    /// <c>fauna_core::scoring::MUTED_KEYWORD_DEFAULT_WEIGHT</c>).
    private uniffi.fauna_core.MutedKeyword[] NextMutedKeywordEntries() =>
        NextMutedKeywords.Select(w => new uniffi.fauna_core.MutedKeyword(w, -1000)).ToArray();

    public Task<MutedWordsSnapshot> MutedKeywordsListAsync()
    {
        _calls.Add("MutedKeywordsList");
        Throw();
        return Task.FromResult(new MutedWordsSnapshot(NextMutedKeywordEntries(), true));
    }

    public Task<MutedWordsSnapshot> MutedKeywordsSetAsync(IReadOnlyList<uniffi.fauna_core.MutedKeyword> keywords)
    {
        _calls.Add("MutedKeywordsSet");
        LastMutedKeywordsSet = keywords;
        Throw();
        return Task.FromResult(new MutedWordsSnapshot(NextMutedKeywordEntries(), true));
    }

    /// The delta pair. The mock records the WORD, not a list —
    /// the seam's whole point is that only the delta crosses; tests set
    /// NextMutedKeywords to model the post-op stored list.
    public string? LastMutedKeywordAdded { get; private set; }
    public string? LastMutedKeywordRemoved { get; private set; }

    public Task<MutedWordsSnapshot> MutedKeywordsAddAsync(string word)
    {
        _calls.Add("MutedKeywordsAdd");
        LastMutedKeywordAdded = word;
        Throw();
        return Task.FromResult(new MutedWordsSnapshot(NextMutedKeywordEntries(), true));
    }

    public Task<MutedWordsSnapshot> MutedKeywordsRemoveAsync(string word)
    {
        _calls.Add("MutedKeywordsRemove");
        LastMutedKeywordRemoved = word;
        Throw();
        return Task.FromResult(new MutedWordsSnapshot(NextMutedKeywordEntries(), true));
    }

    // ── Trained topics ────────────────────────────────────────────────────
    /// The row list each trained-topics call returns (the seam returns the
    /// freshly-persisted rows every time — tests set this to model the post-op state).
    public FfiTrainedTopicRow[] NextTrainedTopics { get; set; } = Array.Empty<FfiTrainedTopicRow>();
    public string? LastTrainedTopicsCreateName { get; private set; }
    public (byte[] Id, string Name)? LastTrainedTopicsRename { get; private set; }
    public byte[]? LastTrainedTopicsDeleteId { get; private set; }
    public (byte[] Id, bool On)? LastTrainedTopicsSetLearnFromEngagement { get; private set; }

    /// Set to hold the LIST call open (never completing on its own) — a test
    /// controls exactly when it resolves via <see cref="TaskCompletionSource{T}.SetResult"/>
    /// / <c>SetException</c>, to reproduce a load that resolves AFTER a faster
    /// gesture (the staleness guard, mirroring tui's tests).
    public TaskCompletionSource<FfiTrainedTopicRow[]>? TrainedTopicsListGate { get; set; }

    public Task<FfiTrainedTopicRow[]> TrainedTopicsListAsync()
    {
        _calls.Add("TrainedTopicsList");
        if (TrainedTopicsListGate is not null) return TrainedTopicsListGate.Task;
        Throw();
        return Task.FromResult(NextTrainedTopics);
    }

    public Task<FfiTrainedTopicRow[]> TrainedTopicsCreateAsync(string name)
    {
        _calls.Add("TrainedTopicsCreate");
        LastTrainedTopicsCreateName = name;
        Throw();
        return Task.FromResult(NextTrainedTopics);
    }

    public Task<FfiTrainedTopicRow[]> TrainedTopicsRenameAsync(byte[] id, string name)
    {
        _calls.Add("TrainedTopicsRename");
        LastTrainedTopicsRename = (id, name);
        Throw();
        return Task.FromResult(NextTrainedTopics);
    }

    public Task<FfiTrainedTopicRow[]> TrainedTopicsDeleteAsync(byte[] id)
    {
        _calls.Add("TrainedTopicsDelete");
        LastTrainedTopicsDeleteId = id;
        Throw();
        return Task.FromResult(NextTrainedTopics);
    }

    public Task<FfiTrainedTopicRow[]> TrainedTopicsSetLearnFromEngagementAsync(byte[] id, bool on)
    {
        _calls.Add("TrainedTopicsSetLearnFromEngagement");
        LastTrainedTopicsSetLearnFromEngagement = (id, on);
        Throw();
        return Task.FromResult(NextTrainedTopics);
    }

    // ── Publishing a trained factor as a List (topic-factors.md § Publishing
    // a trained factor; frame D8) ───────────────────────────────────────────
    public ScoredExemplar[] NextScoredExemplars { get; set; } = Array.Empty<ScoredExemplar>();
    public string? LastScoreCorpusForFactorArg { get; private set; }
    public FfiPublishedList NextPublishedList { get; set; } = new(Array.Empty<byte>(), 1, 0);
    public (byte[] FactorId, string Name, IReadOnlyList<FfiPublishEntry> Entries)? LastTrainedTopicPublishList { get; private set; }

    public Task<ScoredExemplar[]> ScoreCorpusForFactorAsync(string factor)
    {
        _calls.Add("ScoreCorpusForFactor");
        LastScoreCorpusForFactorArg = factor;
        Throw();
        return Task.FromResult(NextScoredExemplars);
    }

    public Task<FfiPublishedList> TrainedTopicPublishListAsync(byte[] factorId, string name, IReadOnlyList<FfiPublishEntry> entries)
    {
        _calls.Add("TrainedTopicPublishList");
        LastTrainedTopicPublishList = (factorId, name, entries);
        Throw();
        return Task.FromResult(NextPublishedList);
    }

    // ── Publishing a trained factor as a Model (topic-factors.md § Publishing
    // a trained factor, v2) — the List section's exact shape. ─────────────────
    public TrainedModelReview NextTrainedModelReview { get; set; } =
        new(0, 0, 0, 0, Array.Empty<ReviewNgram>());
    public string? LastScrubCorpusForFactorArg { get; private set; }
    public FfiPublishedModel NextPublishedModel { get; set; } = new(Array.Empty<byte>(), 1, 0, 0);
    public (byte[] FactorId, string Name, uint MoreDocs, uint LessDocs, IReadOnlyList<FfiPublishNgram> Ngrams)? LastTrainedTopicPublishModel { get; private set; }

    public Task<TrainedModelReview> ScrubCorpusForFactorAsync(string factor)
    {
        _calls.Add("ScrubCorpusForFactor");
        LastScrubCorpusForFactorArg = factor;
        Throw();
        return Task.FromResult(NextTrainedModelReview);
    }

    public Task<FfiPublishedModel> TrainedTopicPublishModelAsync(byte[] factorId, string name, uint moreDocs, uint lessDocs, IReadOnlyList<FfiPublishNgram> ngrams)
    {
        _calls.Add("TrainedTopicPublishModel");
        LastTrainedTopicPublishModel = (factorId, name, moreDocs, lessDocs, ngrams);
        Throw();
        return Task.FromResult(NextPublishedModel);
    }

    public string? NextDefaultConflictPolicy { get; set; }
    public string? LastDefaultConflictPolicySet { get; private set; }

    public Task<string?> DefaultConflictPolicyGetAsync()
    {
        _calls.Add("DefaultConflictPolicyGet");
        Throw();
        return Task.FromResult(NextDefaultConflictPolicy);
    }

    public Task<string?> DefaultConflictPolicySetAsync(string? policy)
    {
        _calls.Add("DefaultConflictPolicySet");
        LastDefaultConflictPolicySet = policy;
        NextDefaultConflictPolicy = policy;
        Throw();
        return Task.FromResult<string?>(policy);
    }

    // ── Subscriptions (profile Tiers-tab SELF author management) ─────────────
    /// The author's own tiers each <c>tiers.list</c> returns (§1).
    public IReadOnlyList<FfiTierItem> NextTiers { get; set; } = new List<FfiTierItem>();
    /// The pending requests each <c>requests.list</c> returns (§2).
    public IReadOnlyList<FfiPendingRequest> NextRequests { get; set; } = new List<FfiPendingRequest>();
    /// The selected tier's roster each <c>subscribers.list</c> returns (§3).
    public IReadOnlyList<FfiSubscriberEntry> NextSubscribers { get; set; } = new List<FfiSubscriberEntry>();
    /// The reply the approve mint+upload returns (defaults to echoing the request).
    public FfiApproveReply? NextApproveReply { get; set; }

    public (string Name, uint Rank, string? Description, string? PriceHint, string? PaymentUrl, bool AutoApprove, ulong? AskingPriceSats)? LastSubscriptionTierCreate { get; private set; }
    public (string Name, uint? Rank, string? Description, string? PriceHint, string? PaymentUrl, bool? AutoApprove, ulong? AskingPriceSats)? LastSubscriptionTierUpdate { get; private set; }
    public string? LastSubscriptionTierDelete { get; private set; }
    public FfiPendingRequest? LastApprovedRequest { get; private set; }
    public long? LastRejectedRequest { get; private set; }
    public string? LastSubscribersListTier { get; private set; }
    public (string TierName, byte[] SubscriberId)? LastSubscriberRemove { get; private set; }

    /// Make only the own-subscription-tiers read fail, so a test can pin the
    /// deliberate asymmetry in the membership section's load: the row-set read
    /// degrades to an empty section, while the designation read does not.
    public bool FailSubscriptionTiersList { get; set; }

    public Task<IReadOnlyList<FfiTierItem>> SubscriptionTiersListAsync()
    {
        _calls.Add("SubscriptionTiersList");
        Throw();
        if (FailSubscriptionTiersList)
            throw new InvalidOperationException("own-tiers read failed");
        return Task.FromResult(NextTiers);
    }

    public Task<bool> SubscriptionTierCreateAsync(
        string name, uint rank, string? description, string? priceHint, string? paymentUrl, bool autoApprove,
        ulong? askingPriceSats)
    {
        _calls.Add("SubscriptionTierCreate");
        LastSubscriptionTierCreate = (name, rank, description, priceHint, paymentUrl, autoApprove, askingPriceSats);
        Throw();
        return Task.FromResult(true);
    }

    public Task<bool> SubscriptionTierUpdateAsync(
        string name, uint? rank, string? description, string? priceHint, string? paymentUrl, bool? autoApprove,
        ulong? askingPriceSats)
    {
        _calls.Add("SubscriptionTierUpdate");
        LastSubscriptionTierUpdate = (name, rank, description, priceHint, paymentUrl, autoApprove, askingPriceSats);
        Throw();
        return Task.FromResult(true);
    }

    public Task<bool> SubscriptionTierDeleteAsync(string name)
    {
        _calls.Add("SubscriptionTierDelete");
        LastSubscriptionTierDelete = name;
        Throw();
        return Task.FromResult(true);
    }

    public Task<IReadOnlyList<FfiPendingRequest>> SubscriptionRequestsListAsync()
    {
        _calls.Add("SubscriptionRequestsList");
        Throw();
        return Task.FromResult(NextRequests);
    }

    public Task<FfiApproveReply> SubscriptionRequestApproveAsync(FfiPendingRequest request)
    {
        _calls.Add("SubscriptionRequestApprove");
        LastApprovedRequest = request;
        Throw();
        return Task.FromResult(NextApproveReply ?? MakeApproveReply(request.subscriberId, request.tierName));
    }

    public Task<bool> SubscriptionRequestRejectAsync(long requestId)
    {
        _calls.Add("SubscriptionRequestReject");
        LastRejectedRequest = requestId;
        Throw();
        return Task.FromResult(true);
    }

    public Task<IReadOnlyList<FfiSubscriberEntry>> SubscriptionSubscribersListAsync(string tierName)
    {
        _calls.Add("SubscriptionSubscribersList");
        LastSubscribersListTier = tierName;
        Throw();
        return Task.FromResult(NextSubscribers);
    }

    public Task SubscriptionSubscriberRemoveAsync(string tierName, byte[] subscriberId)
    {
        _calls.Add("SubscriptionSubscriberRemove");
        LastSubscriberRemove = (tierName, subscriberId);
        Throw();
        return Task.CompletedTask;
    }

    // ── Subscriptions author-side reconciliation ─────────────────────────────
    /// Per-half outcome hooks for the ONE shared tick. The real
    /// <c>reconcile_once</c> reports each half's fault inside the pass rather
    /// than throwing (neither half can abort the other), so these are pass
    /// fields, not exceptions. Mutable mid-test — the pump's delay seam is the
    /// interleave point.
    public string? NextResumeRemovalsError { get; set; }
    public string? NextDrainAutoApprovalsError { get; set; }
    public uint NextResumeRemovalsCount { get; set; }
    public uint NextDrainAutoApprovalsCount { get; set; }
    /// Every pass this mock returned, oldest first — lets a test assert what a
    /// LATER tick observed, which is the whole point of running both halves
    /// every tick.
    public List<FfiReconcilePass> ReconcilePasses { get; } = new();

    public Task<FfiReconcilePass> SubscriptionsReconcileOnceAsync()
    {
        _calls.Add("SubscriptionsReconcileOnce");
        Throw();
        var pass = MakeReconcilePass(
            NextResumeRemovalsCount,
            NextDrainAutoApprovalsCount,
            NextResumeRemovalsError,
            NextDrainAutoApprovalsError);
        ReconcilePasses.Add(pass);
        return Task.FromResult(pass);
    }

    // ── Subscriptions consumer (mine.list + unsubscribe) ─────────────────────
    /// The consumer's own subscriptions each <c>mine.list</c> returns.
    public IReadOnlyList<FfiMineSubscription> NextMineSubscriptions { get; set; } = new List<FfiMineSubscription>();
    /// The unsubscribe reply (defaults to the <c>Removed</c> variant — immediate removal).
    public FfiUnsubscribeReply NextUnsubscribeReply { get; set; } = new FfiUnsubscribeReply.Removed();
    /// Last <c>authorId</c> passed to <see cref="SubscriptionUnsubscribeAsync"/>.
    public byte[]? LastUnsubscribeAuthorId { get; private set; }

    public Task<IReadOnlyList<FfiMineSubscription>> SubscriptionMineListAsync()
    {
        _calls.Add("SubscriptionMineList");
        Throw();
        return Task.FromResult(NextMineSubscriptions);
    }

    public Task<FfiUnsubscribeReply> SubscriptionUnsubscribeAsync(byte[] authorId)
    {
        _calls.Add("SubscriptionUnsubscribe");
        LastUnsubscribeAuthorId = authorId;
        Throw();
        return Task.FromResult(NextUnsubscribeReply);
    }

#if PAYMENTS
    // Gated with the interface half it implements: a store-safe build's INestRpcClient
    // declares no payments members and the generated face has no FfiProviderItem /
    // FfiClaimItem (dynamic-features.md § Platform-family surface excision).
    // ── Payments (Pillar 3 client legs — monetization.md § Pillars 2+3) ──────
    /// The author's own configured providers each <c>providers.list</c> returns (§4).
    public IReadOnlyList<FfiProviderItem> NextProviders { get; set; } = new List<FfiProviderItem>();
    /// The author's own claim codes each <c>claims.list</c> returns (§5).
    public IReadOnlyList<FfiClaimItem> NextClaims { get; set; } = new List<FfiClaimItem>();
    public bool NextProvidersSetResult { get; set; } = true;
    public bool NextProvidersRemoveResult { get; set; } = true;
    public FfiClaimRedeemReply NextClaimRedeemReply { get; set; } = MakeClaimRedeemReply();
    public FfiClaimMintReply NextClaimMintReply { get; set; } = MakeClaimMintReply();

    public (string Kind, string WebhookSecret, string Tier)? LastProvidersSet { get; private set; }
    public string? LastProvidersRemove { get; private set; }
    public string? LastClaimsRedeem { get; private set; }
    public (string Tier, ulong? ValidUntil)? LastClaimsMint { get; private set; }

    public Task<IReadOnlyList<FfiProviderItem>> PaymentsProvidersListAsync()
    {
        _calls.Add("PaymentsProvidersList");
        Throw();
        return Task.FromResult(NextProviders);
    }

    public Task<bool> PaymentsProvidersSetAsync(string kind, string webhookSecret, string tier)
    {
        _calls.Add("PaymentsProvidersSet");
        LastProvidersSet = (kind, webhookSecret, tier);
        Throw();
        return Task.FromResult(NextProvidersSetResult);
    }

    public Task<bool> PaymentsProvidersRemoveAsync(string kind)
    {
        _calls.Add("PaymentsProvidersRemove");
        LastProvidersRemove = kind;
        Throw();
        return Task.FromResult(NextProvidersRemoveResult);
    }

    public Task<FfiClaimRedeemReply> PaymentsClaimsRedeemAsync(string code)
    {
        _calls.Add("PaymentsClaimsRedeem");
        LastClaimsRedeem = code;
        Throw();
        return Task.FromResult(NextClaimRedeemReply);
    }

    public Task<FfiClaimMintReply> PaymentsClaimsMintAsync(string tier, ulong? validUntil)
    {
        _calls.Add("PaymentsClaimsMint");
        LastClaimsMint = (tier, validUntil);
        Throw();
        return Task.FromResult(NextClaimMintReply);
    }

    public Task<IReadOnlyList<FfiClaimItem>> PaymentsClaimsListAsync()
    {
        _calls.Add("PaymentsClaimsList");
        Throw();
        return Task.FromResult(NextClaims);
    }

#endif   // PAYMENTS

    // ── Subscriptions OTHER-profile browse (offers.list + status.get + subscribe) ──
    /// The author's offered tiers each <c>offers.list</c> returns (reuses FfiTierItem).
    public IReadOnlyList<FfiTierItem> NextOffers { get; set; } = new List<FfiTierItem>();
    /// Last <c>authorId</c> passed to <see cref="SubscriptionOffersListAsync"/>.
    public byte[]? LastOffersAuthorId { get; private set; }
    /// The viewer's status each <c>status.get</c> returns (defaults to not-subscribed).
    public FfiSubscriptionStatus NextStatus { get; set; } = MakeStatus();
    /// Last <c>authorId</c> passed to <see cref="SubscriptionStatusGetAsync"/>.
    public byte[]? LastStatusAuthorId { get; private set; }
    /// The subscribe reply (defaults to the <c>Approved</c> variant — plaintext / auto-approve).
    public FfiSubscribeReply NextSubscribeReply { get; set; } = new FfiSubscribeReply.Approved("gold", null);
    /// The (authorId, tier) the last <see cref="SubscriptionSubscribeAsync"/> captured.
    public (byte[] AuthorId, string Tier)? LastSubscribe { get; private set; }

    public Task<IReadOnlyList<FfiTierItem>> SubscriptionOffersListAsync(byte[] authorId)
    {
        _calls.Add("SubscriptionOffersList");
        LastOffersAuthorId = authorId;
        Throw();
        return Task.FromResult(NextOffers);
    }

    public Task<FfiSubscriptionStatus> SubscriptionStatusGetAsync(byte[] authorId)
    {
        _calls.Add("SubscriptionStatusGet");
        LastStatusAuthorId = authorId;
        Throw();
        return Task.FromResult(NextStatus);
    }

    public Task<FfiSubscribeReply> SubscriptionSubscribeAsync(byte[] authorId, string tier)
    {
        _calls.Add("SubscriptionSubscribe");
        LastSubscribe = (authorId, tier);
        Throw();
        return Task.FromResult(NextSubscribeReply);
    }

    /// A <c>FfiSubscriptionStatus</c> fixture (the viewer's status for an author).
    public static FfiSubscriptionStatus MakeStatus(string? tier = null, ulong? expiresAt = null, bool autoApprove = false) =>
        new FfiSubscriptionStatus(tier, expiresAt, autoApprove);

    // ── Profile edit (display-name / bio / links read-modify-write) ──────────
    /// The decoded display + raw body each <c>profile.get</c> returns; <c>null</c>
    /// models an unpublished profile (or a get/decode failure — the seam maps both
    /// to <c>null</c>).
    public ProfileGetResult? NextProfile { get; set; }
    /// When set, <see cref="ProfileSetAsync"/> throws (the publish-error path); the
    /// get path stays driven by <see cref="NextProfile"/> (null = unpublished).
    public bool NextProfileSetThrows { get; set; }
    /// The (display, base-body, avatar, banner) tuple the last
    /// <see cref="ProfileSetAsync"/> captured.
    public (ProfileDisplay Display, byte[]? BaseBody, FfiProfileImageEdit Avatar, FfiProfileImageEdit Banner)?
        LastProfileSet { get; private set; }

    public Task<ProfileGetResult?> ProfileGetAsync(string actorId)
    {
        _calls.Add("ProfileGet");
        Throw();
        return Task.FromResult(NextProfile);
    }

    public Task<ProfileGetResult?> LoadProfileEditBaseAsync()
    {
        _calls.Add("LoadProfileEditBase");
        Throw();
        return Task.FromResult(NextProfile);
    }

    public Task ProfileSetAsync(
        ProfileDisplay edited, byte[]? baseBody, FfiProfileImageEdit avatar, FfiProfileImageEdit banner)
    {
        _calls.Add("ProfileSet");
        LastProfileSet = (edited, baseBody, avatar, banner);
        if (NextProfileSetThrows)
            throw new InvalidOperationException("profile.set failed");
        Throw();
        return Task.CompletedTask;
    }

    /// A <see cref="ProfileGetResult"/> fixture for the profile-edit read path.
    public static ProfileGetResult MakeProfile(
        string? displayName = null, string? bio = null,
        IReadOnlyList<ProfileLinkRow>? links = null, byte[]? rawBody = null) =>
        new ProfileGetResult(
            new ProfileDisplay(displayName, bio, links ?? new List<ProfileLinkRow>()),
            rawBody ?? new byte[] { 0xff });

    /// A <c>FfiMineSubscription</c> fixture (consumer own-subscriptions row).
    /// <paramref name="authorDisplay"/> defaults to what <c>From&lt;MineSubscription&gt;</c>
    /// pre-computes for this fixture, keeping the row realistic; the view model
    /// still re-chooses handle-else-hex locally, and deleting that is the
    /// entrusted swap.
    public static FfiMineSubscription MakeMineSubscription(
        byte[] authorId, string tier = "gold", string status = "active",
        string? handle = null, ulong since = 0, string? authorDisplay = null) =>
        new FfiMineSubscription(authorId, tier, status, handle, since,
            authorDisplay ?? (handle is { Length: > 0 } h
                ? h
                : Convert.ToHexString(authorId).ToLowerInvariant()));

    /// A <c>FfiTierItem</c> fixture (§1 "My tiers" own-read row).
    public static FfiTierItem MakeTier(
        string name, uint rank = 1, string? description = null, string? priceHint = null,
        ulong? askingPriceSats = null, string? paymentUrl = null, bool autoApprove = false,
        ulong createdAt = 0, string? unlocksPost = null) =>
        new FfiTierItem(name, rank, description, priceHint, askingPriceSats, paymentUrl, autoApprove, createdAt, unlocksPost);

    /// A <c>FfiAdminMembershipTier</c> fixture — one membership designation linking an
    /// owned subscription tier to the admitted / lapsed quota tiers
    /// (monetization.md § Pillar 4).
    public static FfiAdminMembershipTier MakeMembershipTier(
        string tierName, string adminTier = "personal", string lapseTier = "free",
        long createdAt = 0) =>
        new FfiAdminMembershipTier(tierName, adminTier, lapseTier, createdAt);

    /// A <c>FfiPendingRequest</c> fixture (§2 pending request).
    public static FfiPendingRequest MakePendingRequest(
        long requestId, byte[] subscriberId, string tierName, string kind = "subscribe", ulong createdAt = 0,
        bool paymentEntitled = false) =>
        new FfiPendingRequest(requestId, subscriberId, tierName, kind, createdAt, null, paymentEntitled);

    /// A <c>FfiSubscriberEntry</c> fixture (§3 roster).
    public static FfiSubscriberEntry MakeSubscriber(byte[] subscriberId, ulong joinedAt = 0) =>
        new FfiSubscriberEntry(subscriberId, joinedAt, null);

#if PAYMENTS
    /// A <c>FfiProviderItem</c> fixture (§4 configured-provider row).
    public static FfiProviderItem MakeProvider(string kind, string tier, ulong createdAt = 0) =>
        new FfiProviderItem(kind, tier, createdAt, lastVerifiedAt: null, lastRejectedAt: null);

    /// A <c>FfiClaimItem</c> fixture (§5 claim-code audit row).
    public static FfiClaimItem MakeClaim(
        string code, string tier, string provider = "manual", ulong? validUntil = null, ulong createdAt = 0,
        byte[]? redeemedBy = null, ulong? redeemedAt = null, ulong? voidedAt = null) =>
        new FfiClaimItem(code, tier, provider, validUntil, createdAt, redeemedBy, redeemedAt, voidedAt);
#endif   // PAYMENTS

    /// <summary>
    /// The folder wizard is rendered directly off the shared machine (the page
    /// holds the <c>FolderWizardMachine</c> and forwards gestures), not through a
    /// VM-over-seam, so no VM unit test exercises this; it would need a live
    /// <c>FfiNestClient</c> the mock can't supply. Unsupported here.
    /// </summary>
    public Task<uniffi.fauna_folders_machine.FolderWizardMachine> BuildFolderWizardMachineAsync(
        uniffi.fauna_folders_machine.FolderWizardObserver observer,
        IReadOnlyList<uniffi.fauna_folders_machine.DeviceOption> availableDevices)
    {
        _calls.Add("BuildFolderWizardMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a FolderWizardMachine (needs a live FfiNestClient).");
    }

    /// <summary>
    /// The Devices page is rendered directly off the shared <c>DevicesMachine</c>
    /// (the page holds it and forwards gestures), not through a VM-over-seam, so
    /// no VM unit test exercises this; it would need a live <c>FfiNestClient</c>
    /// the mock can't supply. Unsupported here (mirrors the wizard above).
    /// </summary>
    public Task<uniffi.fauna_devices_machine.DevicesMachine> BuildDevicesMachineAsync(
        uniffi.fauna_devices_machine.DevicesObserver observer)
    {
        _calls.Add("BuildDevicesMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a DevicesMachine (needs a live FfiNestClient).");
    }

    /// <summary>
    /// The Backups page's snapshot half runs over the shared <c>BackupsMachine</c>,
    /// which needs a live <c>FfiNestClient</c> the mock can't supply. The VM unit
    /// tests fake the machine ITSELF (<c>BackupsMachineFakeBase</c>) and hand it to
    /// <c>BackupsViewModel.AttachMachine</c>, so nothing needs this builder;
    /// unsupported here (mirrors the devices machine above).
    /// </summary>
    public Task<uniffi.fauna_backups_machine.IBackupsMachine> BuildBackupsMachineAsync(
        uniffi.fauna_backups_machine.BackupsObserver observer, string deviceIdHex)
    {
        _calls.Add("BuildBackupsMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a BackupsMachine (needs a live FfiNestClient).");
    }

    /// <summary>Same rationale as <see cref="BuildDevicesMachineAsync"/> above — needs a
    /// live <c>FfiNestClient</c> the mock can't supply.</summary>
    public Task WireDevicesForeignSetsAsync(uniffi.fauna_devices_machine.DevicesMachine devices)
    {
        _calls.Add("WireDevicesForeignSets");
        throw new NotSupportedException(
            "MockNestRpcClient cannot wire foreign sets (needs a live FfiNestClient).");
    }

    /// <summary>Same rationale as <see cref="BuildMediaMachineAsync"/> above — needs a
    /// live <c>FfiNestClient</c> the mock can't supply.</summary>
    public Task WireMediaFollowedFoldersAsync(uniffi.fauna_media_machine.MediaMachine media)
    {
        _calls.Add("WireMediaFollowedFolders");
        throw new NotSupportedException(
            "MockNestRpcClient cannot wire followed folders (needs a live FfiNestClient).");
    }

    /// <summary>Same rationale as <see cref="BuildDevicesMachineAsync"/> above — needs a
    /// live <c>FfiNestClient</c> the mock can't supply.</summary>
    public Task WireDevicesFollowedFoldersAsync(uniffi.fauna_devices_machine.DevicesMachine devices)
    {
        _calls.Add("WireDevicesFollowedFolders");
        throw new NotSupportedException(
            "MockNestRpcClient cannot wire followed folders (needs a live FfiNestClient).");
    }

    /// <summary>Follow a public folder — the shared follow_ops recipe needs a live
    /// <c>FfiNestClient</c> the mock can't supply.</summary>
    public Task<IReadOnlyList<uniffi.fauna_ffi.FfiFollowedFolder>> FoldersFollowPublicAsync(
        string owner, string folderName)
    {
        _calls.Add($"FoldersFollowPublic:{owner}:{folderName}");
        throw new NotSupportedException(
            "MockNestRpcClient cannot follow a public folder (needs a live FfiNestClient).");
    }

    /// <summary>Unfollow — same rationale as <see cref="FoldersFollowPublicAsync"/>.</summary>
    public Task<IReadOnlyList<uniffi.fauna_ffi.FfiFollowedFolder>> FoldersUnfollowPublicAsync(
        string homeNestUrl, long folderId)
    {
        _calls.Add($"FoldersUnfollowPublic:{homeNestUrl}:{folderId}");
        throw new NotSupportedException(
            "MockNestRpcClient cannot unfollow a public folder (needs a live FfiNestClient).");
    }

    /// <summary>
    /// The Media page is rendered directly off the shared <c>MediaMachine</c> (the
    /// page holds it and forwards gestures), not through a VM-over-seam, so no VM
    /// unit test exercises this; it would need a live <c>FfiNestClient</c> the mock
    /// can't supply. Unsupported here (mirrors the devices machine above).
    /// </summary>
    public Task<uniffi.fauna_media_machine.MediaMachine> BuildMediaMachineAsync(
        uniffi.fauna_media_machine.MediaObserver observer)
    {
        _calls.Add("BuildMediaMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a MediaMachine (needs a live FfiNestClient).");
    }

    /// <summary>
    /// The Personalization + Community-labelers pages are rendered directly off
    /// the shared <c>LabelerCatalogMachine</c> (each page holds its own instance
    /// and forwards gestures), not through a VM-over-seam, so no VM unit test
    /// exercises this; it would need a live <c>FfiNestClient</c> the mock can't
    /// supply. Unsupported here (mirrors the devices/media machines above).
    /// </summary>
    public Task<uniffi.fauna_labeler_catalog_machine.LabelerCatalogMachine> BuildLabelerCatalogMachineAsync(
        uniffi.fauna_labeler_catalog_machine.LabelerCatalogObserver observer)
    {
        _calls.Add("BuildLabelerCatalogMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a LabelerCatalogMachine (needs a live FfiNestClient).");
    }

    /// <summary>
    /// Unsupported here, deliberately: <see cref="FaunaApp.Core.ViewModels.AtprotoViewModel"/>
    /// has a second constructor taking <c>IAtprotoSettingsMachine</c> directly, so its unit
    /// tests drive a FAKE machine rather than routing through this seam (memory
    /// <c>reference_windows_vm_over_uniffi_machine_interface</c>). Building a real one needs
    /// a live <c>FfiNestClient</c> the mock can't supply — same as the machines above.
    /// </summary>
    public Task<uniffi.fauna_client_connected_apps.IConnectedAppsMachine> BuildConnectedAppsMachineAsync(
        uniffi.fauna_client_connected_apps.ConnectedAppsObserver observer,
        uniffi.fauna_client_mail_settings.MailSettingsMachine? mail)
    {
        _calls.Add("BuildConnectedAppsMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a ConnectedAppsMachine (needs a live FfiNestClient); "
            + "use the ConnectedAppsViewModel(IConnectedAppsMachine) test constructor instead.");
    }

    public Task<uniffi.fauna_atproto_settings_machine.IAtprotoSettingsMachine> BuildAtprotoSettingsMachineAsync(
        uniffi.fauna_atproto_settings_machine.AtprotoSettingsObserver observer)
    {
        _calls.Add("BuildAtprotoSettingsMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build an AtprotoSettingsMachine (needs a live FfiNestClient); "
            + "use the AtprotoViewModel(IAtprotoSettingsMachine) test constructor instead.");
    }

    public Task<uniffi.fauna_client_mail_settings.MailPolicyMachine> BuildMailPolicyMachineAsync()
    {
        _calls.Add("BuildMailPolicyMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a MailPolicyMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.CaldavPolicyMachine> BuildCaldavPolicyMachineAsync()
    {
        _calls.Add("BuildCaldavPolicyMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a CaldavPolicyMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.CarddavPolicyMachine> BuildCarddavPolicyMachineAsync()
    {
        _calls.Add("BuildCarddavPolicyMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a CarddavPolicyMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.WebdavPolicyMachine> BuildWebdavPolicyMachineAsync()
    {
        _calls.Add("BuildWebdavPolicyMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a WebdavPolicyMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.ForwarderMachine> BuildForwardersMachineAsync()
    {
        _calls.Add("BuildForwardersMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a ForwarderMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.BridgeApprovalMachine> BuildBridgeApprovalMachineAsync()
    {
        _calls.Add("BuildBridgeApprovalMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a BridgeApprovalMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_dns.DnsManagementMachine> BuildDnsManagementMachineWithCredentialsAsync()
    {
        _calls.Add("BuildDnsManagementMachineWithCredentials");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a DnsManagementMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.LocalDomainMachine> BuildLocalDomainsMachineAsync()
    {
        _calls.Add("BuildLocalDomainsMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a LocalDomainMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_pair.LinkedNestsMachine> BuildLinkedNestsMachineAsync()
    {
        _calls.Add("BuildLinkedNestsMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a LinkedNestsMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_ffi.FfiWebClient> BuildWebClientAsync()
    {
        _calls.Add("BuildWebClient");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build an FfiWebClient (needs a live FfiNestClient).");
    }

    public Task<string> AdminFactoryResetAsync(string? newClaimCode)
    {
        _calls.Add("AdminFactoryReset");
        throw new NotSupportedException(
            "MockNestRpcClient cannot run a factory reset (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_pair.LinkedNestsMachine> BuildLinkedNestsMachineWithMailRelayAsync()
    {
        _calls.Add("BuildLinkedNestsMachineWithMailRelay");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a LinkedNestsMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_pair.LinkedNestsMachine> BuildLinkedNestsMachineWithMailRelayAndTrustAsync()
    {
        _calls.Add("BuildLinkedNestsMachineWithMailRelayAndTrust");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a LinkedNestsMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.MailSettingsMachine> BuildMailSettingsMachineAsync()
    {
        _calls.Add("BuildMailSettingsMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a MailSettingsMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.MailSpamMachine> BuildMailSpamMachineAsync()
    {
        _calls.Add("BuildMailSpamMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a MailSpamMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.MailListsMachine> BuildMailListsMachineAsync()
    {
        _calls.Add("BuildMailListsMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a MailListsMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.MailListMembersMachine> BuildMailListMembersMachineAsync(string listIdHex, string listName)
    {
        _calls.Add("BuildMailListMembersMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a MailListMembersMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.MailExportMachine> BuildMailExportMachineAsync(string handle, string saveDir)
    {
        _calls.Add("BuildMailExportMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a MailExportMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.MailImportMachine> BuildMailImportMachineAsync()
    {
        _calls.Add("BuildMailImportMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a MailImportMachine (needs a live FfiNestClient).");
    }

    public Task<uniffi.fauna_client_mail_settings.MailAliasesMachine> BuildMailAliasesMachineAsync()
    {
        _calls.Add("BuildMailAliasesMachine");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a MailAliasesMachine (needs a live FfiNestClient).");
    }

    /// <summary>
    /// The conversations page renders off the shared <c>ConversationsSession</c>
    /// in production, but the test double has no live <c>FfiNestClient</c> to
    /// build one from — VM unit tests drive a <c>ConversationsManager</c>
    /// directly (the E2E / no-session fallback path). Returns <c>null</c>
    /// (page falls back to <c>ConversationsManagerHost.Instance</c>).
    /// </summary>
    public Task<ConversationsSession?> BuildConversationsSessionAsync(
        ConversationsManager manager, string? identityDomain = null, string? deviceIdHex = null)
    {
        _ = manager;
        _ = identityDomain;
        _ = deviceIdHex;
        _calls.Add("BuildConversationsSession");
        return Task.FromResult<ConversationsSession?>(null);
    }

    /// <summary>
    /// Unsupported in the mock: the provisioner needs a live, connected
    /// <c>FfiNestClient</c> the mock can't supply (mirrors
    /// <see cref="BuildFeedManagerAsync"/>).
    /// </summary>
    public Task<FfiSyncAgentProvisioner> BuildSyncAgentProvisionerAsync(
        byte[][] predecessorBackupKeys,
        byte[][] predecessorActorIds,
        string deviceId,
        string deviceLabel,
        FfiAgentSpawner spawner,
        FfiProvisioningBearerSource bearerSource,
        FfiAgentReachabilityObserver? reachabilityObserver)
    {
        _calls.Add("BuildSyncAgentProvisioner");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a FfiSyncAgentProvisioner (needs a live FfiNestClient).");
    }

    /// <summary>What <see cref="ReseedCustodianStoreAsync"/> answers — the agent
    /// face itself needs a live <c>FfiNestClient</c>, so the mock stands in for
    /// the whole pass-through.</summary>
    public FfiReseedResult? NextReseedResult { get; set; }

    /// <summary>Thrown by <see cref="ReseedCustodianStoreAsync"/> when set.</summary>
    public Exception? NextReseedException { get; set; }

    /// <summary>The device ids each re-seed was asked with, in order.</summary>
    public List<string> ReseedDeviceIds { get; } = new();

    public Task<FfiReseedResult> ReseedCustodianStoreAsync(
        IFfiSyncAgentProvisioner agent, string thisDeviceId)
    {
        _ = agent;
        _calls.Add("ReseedCustodianStore");
        ReseedDeviceIds.Add(thisDeviceId);
        if (NextReseedException is not null) throw NextReseedException;
        return Task.FromResult(NextReseedResult
            ?? throw new InvalidOperationException("MockNestRpcClient: set NextReseedResult first"));
    }

    /// <summary>
    /// Unsupported in the mock: the FeedManager needs a live <c>FfiNestClient</c>
    /// the mock can't supply (mirrors the page-machine factories). The Feed page's
    /// snapshot consume is exercised by the tier_3 e2e, not a VM unit test.
    /// </summary>
    public Task<uniffi.fauna_ffi.FfiFeedManager> BuildFeedManagerAsync()
    {
        _calls.Add("BuildFeedManager");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a FfiFeedManager (needs a live FfiNestClient).");
    }

    /// <summary>
    /// Unsupported in the mock: the SearchManager needs a live <c>FfiNestClient</c>
    /// the mock can't supply (mirrors <see cref="BuildFeedManagerAsync"/>). The
    /// Search page's snapshot consume is exercised by the tier_3 e2e, not a VM
    /// unit test.
    /// </summary>
    public Task<uniffi.fauna_ffi.FfiSearchManager> BuildSearchManagerAsync()
    {
        _calls.Add("BuildSearchManager");
        throw new NotSupportedException(
            "MockNestRpcClient cannot build a FfiSearchManager (needs a live FfiNestClient).");
    }

    /// <summary>
    /// Unsupported in the mock: registering the local arm needs a live
    /// <c>FfiSearchManager</c> the mock can't fabricate either (mirrors
    /// <see cref="BuildSearchManagerAsync"/>). Exercised by the tier_3 e2e
    /// (test_search.py, test_search_local_index.py), not a VM unit test.
    /// </summary>
    public Task<bool> AttachLocalSearchIndexAsync(uniffi.fauna_ffi.FfiSearchManager manager)
    {
        _calls.Add("AttachLocalSearchIndex");
        throw new NotSupportedException(
            "MockNestRpcClient cannot attach a local search index (needs a live FfiSearchManager).");
    }

    // ── fixture helpers ─────────────────────────────────────────────────

    public static FfiNotifItem MakeNotif(long id, string type, string summary, bool isRead, long createdAt) =>
        new FfiNotifItem(id, type, "fauna", null, null, null, summary, isRead, createdAt);

    public static FfiKnock MakeKnock(string senderId, string summary, LocalizedText? body = null) =>
        new FfiKnock(senderId, summary, body);

    // ── fixture helpers (events / quota) ────────────────────────────────

    public static FfiQuotaGetReply MakeQuota(long storageUsed, long storageMax) =>
        new FfiQuotaGetReply(
            "personal",
            new FfiUsageBytes(0, 0),
            new FfiUsageBytes(storageUsed, storageMax),
            new FfiQuotaDeviceUsage(0, 0),
            new FfiQuotaFeatures(false, false, 0));

    public static FfiCalendarRow MakeCaldavCalendar(string idHex, string name, string color = "") =>
        new FfiCalendarRow(idHex, name, color);

    public static FfiCalAttendee MakeCaldavAttendee(string email, string rsvp = "invited", string name = "") =>
        new FfiCalAttendee(email, name, rsvp);

    public static FfiCalEvent MakeCaldavEvent(
        string idHex, string summary, string dtstart, string? dtend = null,
        bool organizedByMe = true, string? reminder = null, FfiCalAttendee[]? attendees = null,
        string calendarIdHex = "") =>
        new FfiCalEvent(
            idHex, "uid", calendarIdHex, summary, dtstart, dtend, null, null, "",
            organizedByMe, reminder, attendees ?? Array.Empty<FfiCalAttendee>());

    // ── fixture helpers (admin) ─────────────────────────────────────────

    public static FfiAdminUser MakeAdminUser(
        string actorIdHex, string tier, string label = "", string? handle = null, bool suspended = false,
        FfiAdminEviction? eviction = null, bool mailServingEnabled = true, bool isAdmin = false) =>
        new FfiAdminUser(
            Convert.FromHexString(actorIdHex), tier, label, handle, suspended, 0, 0, 0, eviction, mailServingEnabled, isAdmin);

    /// A pending eviction to seed a row whose <c>AdminUserRow.EvictionActive</c> is
    /// true (the evict→cancel row flip). <paramref name="status"/> defaults to
    /// <c>"scheduled"</c>; pass <c>"warning"</c>/<c>"suspended"</c> to exercise the
    /// <c>admin_user_row_controls</c> lifecycle-state crossing (admin.md § 2 Users).
    public static FfiAdminEviction MakeAdminEviction(
        string reason = "Evicted by admin", string category = "other", string status = "scheduled") =>
        new FfiAdminEviction(status, reason, category, null, null, null);

    public static FfiAdminTier MakeAdminTier(string name) =>
        new FfiAdminTier(name, 0, 0, 0, 0, 0);

    /// A <c>fauna.admin.stats</c> fixture (the admin dashboard's summary row).
    public static FfiAdminStats MakeAdminStats(
        long totalUsers = 0, FfiAdminTierCount[]? usersByTier = null, long suspendedUsers = 0,
        long totalInboxBytes = 0, long totalStorageBytes = 0, long wsConnections = 0) =>
        new FfiAdminStats(
            totalUsers, usersByTier ?? Array.Empty<FfiAdminTierCount>(), suspendedUsers,
            totalInboxBytes, totalStorageBytes, wsConnections);

    /// A <c>fauna.admin.status</c> fixture (version + optional pending update).
    public static FfiAdminStatus MakeAdminStatus(
        string version = "0.0.0", FfiAdminUpdateAvailable? updateAvailable = null) =>
        new FfiAdminStatus(version, updateAvailable);

    /// A <c>fauna.admin.users.list</c> reply fixture.
    public static FfiAdminUsersListReply MakeAdminUsersListReply(FfiAdminUser[]? users = null, long total = 0) =>
        new FfiAdminUsersListReply(users ?? Array.Empty<FfiAdminUser>(), total);

    /// A <c>fauna.admin.invite_requests.approve</c> reply fixture.
    public static FfiAdminInviteRequestApproveReply MakeAdminInviteRequestApproveReply(
        byte[]? actorId = null, string handle = "", string tier = "") =>
        new FfiAdminInviteRequestApproveReply(actorId ?? new byte[32], handle, tier);

    /// A <c>fauna_log::LogEntry</c> fixture (the nest-ring rows the admin Logs page renders).
    public static LogEntry MakeLogEntry(
        ulong timestampMs, LogLevel level, string target = "fauna_nest", string message = "msg") =>
        new LogEntry(timestampMs, level, target, message);

    /// A <c>fauna.setup.status</c> reply with the given storage <paramref name="mode"/>
    /// (<c>"Plaintext"</c>/<c>"Encrypted"</c>/null); the other fields are unset.
    /// The trailing values mirror the nest serde defaults (discovery.rs SetupStatusReply):
    /// <c>mail_subsystem_ok</c> (healthy), <c>auto_enable_mail_for_new_users</c> (default-on),
    /// <c>subhandles</c> (default-off),
    /// <c>serving_port</c> (default 443), <c>fronted_by_router</c> (default false =
    /// a direct-listener desktop / bare-IP box where serving_port is a genuine
    /// admin choice; <c>true</c> = a router-fronted Docker/cloud nest).
    /// The trailing host-OS-maintenance fields (<c>os_security_updates_pending</c>,
    /// <c>os_reboot_pending</c>, and the informational-only <c>os_reboot_deferred_since</c>
    /// / <c>os_last_patched_at</c> — not rendered in v1) default to the nest serde
    /// "nothing pending" state (0 / false / None), i.e. a nest with no host channel
    /// (dev / desktop) → "OS up to date", no false alarm
    /// (installers/vps.md § Host OS Maintenance § 4).
    // NAMED args deliberately: this record grows from the shared side, and two
    // fields (registrationMode, maxFreeUsers) were once inserted MID-list, which
    // silently shifted every positional arg after them. Named args make that
    // class of misalignment a compile error instead of a wrong fixture value.
    public static FfiSetupStatus MakeSetupStatus(
        ushort servingPort = 443, bool frontedByRouter = false,
        uint osSecurityUpdatesPending = 0, bool osRebootPending = false) =>
        new FfiSetupStatus(
            domain: "nest.test", dnsConfigured: false, tlsActive: false, emailEnabled: false,
            adminExists: true, claimed: true, version: "0.0.0",
            mailSubsystemOk: true, autoEnableMailForNewUsers: true,
            registrationMode: null, maxFreeUsers: null, subhandles: false,
            ageVerificationRequired: false, maxStorageBytes: null,
            corsOrigins: Array.Empty<string>(), servingPort: servingPort, frontedByRouter: frontedByRouter,
            osSecurityUpdatesPending: osSecurityUpdatesPending, osRebootPending: osRebootPending,
            osRebootDeferredSince: null, osLastPatchedAt: null,
            webAppOrigin: "bundled", webAppOriginTarget: null, webAppOriginDomainless: false);

    public static FfiAdminInviteCode MakeAdminInviteCode(string code, string tier, long usesLeft = 1) =>
        new FfiAdminInviteCode(code, tier, usesLeft, 0, null);

    public static FfiAdminInviteRequest MakeAdminInviteRequest(
        long id, string handle, string actorIdHex, string message = "let me in",
        string status = "pending") =>
        new FfiAdminInviteRequest(
            id, Convert.FromHexString(actorIdHex), handle, message, status,
            status == "pending", 0, null, null, null, null, null);

    // ── CardDAV Address Book (contacts.md § Address Book segment; slice 4b) ──

    /// The <c>list_addressbooks</c> fixture.
    public FfiAddressbookRow[] NextAddressbooks { get; set; } = Array.Empty<FfiAddressbookRow>();
    /// The <c>query_cards</c> fixture.
    public FfiCardRow[] NextCards { get; set; } = Array.Empty<FfiCardRow>();
    /// Last <c>query_cards</c> addressbook id hex.
    public string? LastCardsQueryAddressbookIdHex { get; private set; }

    public Task<FfiAddressbookRow[]> CarddavListAddressbooksAsync()
    {
        _calls.Add("CarddavListAddressbooks");
        Throw();
        return Task.FromResult(NextAddressbooks);
    }

    public Task<FfiCardRow[]> CarddavQueryCardsAsync(string addressbookIdHex)
    {
        _calls.Add("CarddavQueryCards");
        LastCardsQueryAddressbookIdHex = addressbookIdHex;
        Throw();
        return Task.FromResult(NextCards);
    }

    /// The `locate_card_by_uid_hash` fixture — defaults to "no book holds it"
    /// (empty books, `found: null`), the same DROPPED-outcome default every
    /// unresolvable-hit fixture in this file takes.
    public FfiLocatedCard NextLocatedCard { get; set; } = new(Array.Empty<FfiAddressbookRow>(), null);
    /// Last `locate_card_by_uid_hash` uid_hash hex.
    public string? LastLocateCardUidHashHex { get; private set; }

    public Task<FfiLocatedCard> CarddavLocateCardByUidHashAsync(string uidHashHex)
    {
        _calls.Add("CarddavLocateCardByUidHash");
        LastLocateCardUidHashHex = uidHashHex;
        Throw();
        return Task.FromResult(NextLocatedCard);
    }

    /// <summary>
    /// Build a <see cref="FfiAddressbookRow"/> fixture (mirrors linux
    /// <c>address_book_row</c> field shape).
    /// </summary>
    public static FfiAddressbookRow MakeAddressbookRow(string idHex, string name, uint cardCount = 0) =>
        new FfiAddressbookRow(idHex, name, "", cardCount);

    // ── Task delegation ──────────────────────────────────────────────────

    public FfiTaskDelegationRow[] NextTaskDelegationRows { get; set; } = Array.Empty<FfiTaskDelegationRow>();
    public string? LastTaskDelegationListDeviceId { get; private set; }
    public (string deviceId, string taskKind, FfiPinOption option)? LastTaskDelegationSetAssignment { get; private set; }

    public Task<FfiTaskDelegationRow[]> TaskDelegationListAsync(string deviceId)
    {
        _calls.Add("TaskDelegationList");
        LastTaskDelegationListDeviceId = deviceId;
        Throw();
        return Task.FromResult(NextTaskDelegationRows);
    }

    public Task TaskDelegationSetAssignmentAsync(string deviceId, string taskKind, FfiPinOption option)
    {
        _calls.Add("TaskDelegationSetAssignment");
        LastTaskDelegationSetAssignment = (deviceId, taskKind, option);
        Throw();
        return Task.CompletedTask;
    }

    // ── Recovery kit + succession ────────────────────────────────────────

    /// <summary>What <see cref="RecoveryKitStatusAsync"/> hands back; defaults to
    /// the never-created state with the shared enablement predicates' real answers
    /// (create + stolen only — <c>allows_stolen</c> is unconditionally true).</summary>
    public FfiRecoveryKitStatus NextRecoveryKitStatus { get; set; } =
        MakeRecoveryKitStatus("never-created", allowsCreate: true);

    /// <summary>What the three minting ceremonies hand back.</summary>
    public FfiMintedKit NextMintedKit { get; set; } = new FfiMintedKit(new string('a', 64), true, null);

    /// <summary>What <see cref="SuccessionSucceedWithHeldKitAsync"/> hands back.</summary>
    public FfiLandedSuccession NextLandedSuccession { get; set; } = MakeLandedSuccession();

    /// <summary>What <see cref="RecoveryVetoPendingReplacementAsync"/> reports.</summary>
    public bool NextVetoCancelledSomething { get; set; } = true;

    public string? LastRecoveryCreateHeldKitInput { get; private set; }
    public string? LastRecoveryVetoHeldKitInput { get; private set; }
    public string? LastRecoveryResealHeldKitInput { get; private set; }
    public string? LastSuccessionKitInput { get; private set; }

    public Task<FfiRecoveryKitStatus> RecoveryKitStatusAsync()
    {
        _calls.Add("RecoveryKitStatus");
        Throw();
        return Task.FromResult(NextRecoveryKitStatus);
    }

    public Task<FfiMintedKit> RecoveryCreateKitAsync(string? heldKitInput)
    {
        _calls.Add("RecoveryCreateKit");
        LastRecoveryCreateHeldKitInput = heldKitInput;
        Throw();
        return Task.FromResult(NextMintedKit);
    }

    public Task<FfiMintedKit> RecoveryRequestSeedAloneReplacementAsync()
    {
        _calls.Add("RecoveryRequestSeedAloneReplacement");
        Throw();
        return Task.FromResult(NextMintedKit);
    }

    public Task<bool> RecoveryVetoPendingReplacementAsync(string heldKitInput)
    {
        _calls.Add("RecoveryVetoPendingReplacement");
        LastRecoveryVetoHeldKitInput = heldKitInput;
        Throw();
        return Task.FromResult(NextVetoCancelledSomething);
    }

    public Task<long> RecoveryResealEscrowWithHeldKitAsync(string heldKitInput)
    {
        _calls.Add("RecoveryResealEscrowWithHeldKit");
        LastRecoveryResealHeldKitInput = heldKitInput;
        Throw();
        return Task.FromResult(1_700_000_000L);
    }

    public Task<FfiLandedSuccession> SuccessionSucceedWithHeldKitAsync(string kitInput)
    {
        _calls.Add("SuccessionSucceedWithHeldKit");
        LastSuccessionKitInput = kitInput;
        Throw();
        return Task.FromResult(NextLandedSuccession);
    }

    /// <summary>
    /// A recovery-kit status fixture. The four <c>allows*</c> flags are passed
    /// explicitly rather than derived from <paramref name="kind"/> ON PURPOSE —
    /// they are shared predicates that deliberately do NOT follow from the kind
    /// (<c>allows_stolen</c> is unconditionally true; <c>allows_replace</c> stays
    /// true during a pending window), and a mock that re-derived them would let a
    /// view model quietly do the same.
    /// </summary>
    public static FfiRecoveryKitStatus MakeRecoveryKitStatus(
        string kind, bool allowsCreate = false, bool allowsReplace = false,
        bool allowsLost = false, bool allowsStolen = true, bool allowsEscrowReseal = false,
        string? pendingNewPubkeyHex = null, long? pendingLandsAt = null) =>
        new FfiRecoveryKitStatus(
            kind, allowsCreate, allowsReplace, allowsLost, allowsStolen, allowsEscrowReseal,
            pendingNewPubkeyHex, pendingLandsAt);

    /// <summary>A landed-succession fixture; <paramref name="persisted"/> false is
    /// the arm where the secret must go on screen and the session must survive.</summary>
    /// <summary>What <see cref="RunSuccessionAftermathAsync"/> reports. The
    /// overwhelmingly common real answer, so it is the default.</summary>
    public FfiAftermathOutcome NextAftermathOutcome { get; set; } =
        FfiAftermathOutcome.NotASuccessor;

    public Task<FfiAftermathOutcome> RunSuccessionAftermathAsync()
    {
        _calls.Add("RunSuccessionAftermath");
        Throw();
        return Task.FromResult(NextAftermathOutcome);
    }

    /// <summary>What <see cref="SuccessionRetryGroupSweepAsync"/> hands back. The
    /// "no-old-state" default is the ordinary answer on a device that holds no
    /// conversation history for the retired identity.</summary>
    public FfiSweepRetryAnswer NextSweepRetryAnswer { get; set; } =
        new FfiSweepRetryAnswer(
            "no-old-state",
            new LocalizedText("settings.recovery_kit.sweep_retry_no_old_state", new()),
            null, null, Array.Empty<byte[]>());

    public Task<FfiSweepRetryAnswer> SuccessionRetryGroupSweepAsync()
    {
        _calls.Add("SuccessionRetryGroupSweep");
        Throw();
        return Task.FromResult(NextSweepRetryAnswer);
    }

    /// <summary>What <see cref="SuccessionDischargeOwedSweepAsync"/> hands back —
    /// by default the no-old-state press, parking an arm that still owes work.</summary>
    public FfiOwedSweepAnswer NextOwedSweepAnswer { get; set; } =
        new FfiOwedSweepAnswer(
            new FfiSweepRetryAnswer(
                "no-old-state",
                new LocalizedText("settings.recovery_kit.sweep_retry_no_old_state", new()),
                null, null, Array.Empty<byte[]>()),
            new FfiSweepView("no-engine", null, 0, 0, 0, true),
            "{\"kind\":\"no_engine\"}");

    public Task<FfiOwedSweepAnswer> SuccessionDischargeOwedSweepAsync()
    {
        _calls.Add("SuccessionDischargeOwedSweep");
        Throw();
        return Task.FromResult(NextOwedSweepAnswer);
    }

    // ── T16 custody facet, owner side ──

    /// <summary>What <see cref="CustodyFacetLoadAsync"/> returns. Null by
    /// default (the shape a mocked test never opted into custody rows sees).</summary>
    public CustodyFacetView? NextCustodyFacet { get; set; }

    /// <summary>What <see cref="CustodyRevokeAsync"/> returns.</summary>
    public FfiCustodyActOutcome NextCustodyActOutcome { get; set; } = new(null, null);

    /// <summary>Last <see cref="CustodyRevokeAsync"/> args, for asserting the
    /// grant id / holder reached the call.</summary>
    public (byte[] GrantId, byte[]? Holder)? LastCustodyRevoke { get; private set; }

    /// <summary>True once <see cref="CustodyDriveAsync"/> has been called.</summary>
    public bool CustodyDriveCalled { get; private set; }

    public Task<CustodyFacetView?> CustodyFacetLoadAsync()
    {
        _calls.Add("CustodyFacetLoad");
        Throw();
        return Task.FromResult(NextCustodyFacet);
    }

    // ── Devices page: the standing enrollment notice ──

    /// <summary>What <see cref="AccountEnrollmentNoticeAsync"/> returns — the
    /// already-localized sentence the account runtime's slot records, or null
    /// (nothing stands, the default a test that never opted in sees).</summary>
    public string? NextEnrollmentNotice { get; set; }

    public Task<string?> AccountEnrollmentNoticeAsync()
    {
        _calls.Add("AccountEnrollmentNotice");
        Throw();
        return Task.FromResult(NextEnrollmentNotice);
    }

    /// <summary>What <see cref="ThisDeviceRowAsync"/> returns when set; null
    /// (the default) echoes the own id — the shared rule's no-enrollment
    /// fallback.</summary>
    public string? NextThisDeviceRow { get; set; }

    public Task<string?> ThisDeviceRowAsync(string? ownDeviceId)
    {
        _calls.Add("ThisDeviceRow");
        Throw();
        return Task.FromResult(NextThisDeviceRow ?? ownDeviceId);
    }

    public Task<FfiCustodyActOutcome> CustodyRevokeAsync(byte[] grantId, byte[]? holder)
    {
        _calls.Add("CustodyRevoke");
        LastCustodyRevoke = (grantId, holder);
        Throw();
        return Task.FromResult(NextCustodyActOutcome);
    }

    public Task CustodyDriveAsync(ConversationsSession session)
    {
        _calls.Add("CustodyDrive");
        CustodyDriveCalled = true;
        Throw();
        return Task.CompletedTask;
    }

    /// <summary>Every custody act's args, in call order: (act name, grant id,
    /// the act's extra arg — onNest / cap / channel hex — or null).</summary>
    public List<(string Act, byte[] GrantId, object? Extra)> CustodyActs { get; } = new();

    /// <summary>What <see cref="CustodyOfferShowsTargetSelectAsync"/> answers,
    /// per offer (default: no select).</summary>
    public Func<CustodyOfferRowView, bool> CustodyShowsTargetSelect { get; set; } = _ => false;

    /// <summary>What <see cref="CustodyMintCandidates"/> returns.</summary>
    public CustodyMintCandidateView[] NextCustodyMintCandidates { get; set; } = Array.Empty<CustodyMintCandidateView>();

    /// <summary>What <see cref="DevicesKeylessPostureAsync"/> answers per
    /// principal (default: no row is keyless).</summary>
    public Func<string?, bool> KeylessPrincipal { get; set; } = _ => false;

    private Task<FfiCustodyActOutcome> CustodyAct(string act, byte[] grantId, object? extra)
    {
        _calls.Add(act);
        CustodyActs.Add((act, grantId, extra));
        Throw();
        return Task.FromResult(NextCustodyActOutcome);
    }

    public Task<FfiCustodyActOutcome> CustodyAcceptAsync(ConversationsSession session, byte[] grantId, bool onNest)
        => CustodyAct("CustodyAccept", grantId, onNest);

    public Task<FfiCustodyActOutcome> CustodyDeclineAsync(byte[] grantId)
        => CustodyAct("CustodyDecline", grantId, null);

    public Task<FfiCustodyActOutcome> CustodySetBudgetAsync(byte[] grantId, ulong cap)
        => CustodyAct("CustodySetBudget", grantId, cap);

    public Task<FfiCustodyActOutcome> CustodyStopAsync(byte[] grantId)
        => CustodyAct("CustodyStop", grantId, null);

    public Task<FfiCustodyActOutcome> CustodyRemoveAsync(byte[] grantId)
        => CustodyAct("CustodyRemove", grantId, null);

    public Task<bool> CustodyOfferShowsTargetSelectAsync(CustodyOfferRowView offer)
    {
        _calls.Add("CustodyOfferShowsTargetSelect");
        Throw();
        return Task.FromResult(CustodyShowsTargetSelect(offer));
    }

    public IReadOnlyList<CustodyMintCandidateView> CustodyMintCandidates(ConversationsSession session)
    {
        _calls.Add("CustodyMintCandidates");
        Throw();
        return NextCustodyMintCandidates;
    }

    public Task<FfiCustodyActOutcome> CustodyMintAsync(ConversationsSession session, byte[] host, string channelHex)
        => CustodyAct("CustodyMint", host, channelHex);

    public Task<IReadOnlyList<bool>> DevicesKeylessPostureAsync(IReadOnlyList<string?> principals)
    {
        _calls.Add("DevicesKeylessPosture");
        Throw();
        return Task.FromResult<IReadOnlyList<bool>>(principals.Select(KeylessPrincipal).ToList());
    }

    public static FfiLandedSuccession MakeLandedSuccession(
        string? successorSecretHex = null, string? newActorIdHex = null, bool persisted = true,
        string sweepKind = "ran", string? sweepDetail = null, string? sweepStateJson = null,
        long? succeededAt = 1_700_000_000L, byte[][]? reviewRoster = null,
        // groups/groupsOldLeafRemoved/unattestedMembers/owesWork: added
        // 2026-08-27 — SweepView::render_view's
        // counts, `0`/`false` by default since no existing caller here asserts on
        // them.
        uint groups = 0, uint groupsOldLeafRemoved = 0, uint unattestedMembers = 0,
        bool owesWork = false) =>
        new FfiLandedSuccession(
            successorSecretHex ?? new string('b', 64),
            newActorIdHex ?? new string('c', 64),
            persisted,
            new FfiSweepView(sweepKind, sweepDetail, groups, groupsOldLeafRemoved, unattestedMembers, owesWork),
            // The sweep's OWN reported roster - the raw 32-byte actor ids, in the
            // shape it handed back. Empty is meaningful (a sweep that found
            // nobody), which is why the default is empty rather than null.
            reviewRoster ?? Array.Empty<byte[]>(),
            sweepStateJson ?? "{\"kind\":\"ran\"}",
            succeededAt);

    /// <summary>
    /// Build a <see cref="FfiCardRow"/> fixture with the common vCard fields
    /// pre-wrapped (mirrors linux <c>vcard_row</c>'s flattened shape).
    /// </summary>
    public static FfiCardRow MakeCardRow(
        string idHex, string formattedName, string[]? emails = null, string[]? tels = null,
        string[]? addresses = null, string[]? org = null, string title = "", string note = "") =>
        new FfiCardRow(
            idHex, "uid", formattedName,
            (emails ?? Array.Empty<string>()).Select(v => new FfiVCardValue(v, Array.Empty<string>(), false)).ToArray(),
            (tels ?? Array.Empty<string>()).Select(v => new FfiVCardValue(v, Array.Empty<string>(), false)).ToArray(),
            (addresses ?? Array.Empty<string>()).Select(a => new FfiPostalAddress(
                Array.Empty<string>(), false, "", "", "", "", "", "", "", a)).ToArray(),
            Array.Empty<FfiVCardValue>(),
            org ?? Array.Empty<string>(), title, note, "", false);

    // ── fixture helpers (spam / folders / search) ────────────────────────

    /// A <c>fauna.spam.preferences.set</c> reply fixture (also this mock's
    /// <see cref="NextSpamPreferences"/> field default).
    public static FfiSpamPreferences MakeSpamPreferences(
        ushort spamThreshold = 500, ushort phishingThreshold = 500) =>
        new FfiSpamPreferences(spamThreshold, phishingThreshold);

    /// A <c>fauna.folders.create</c> reply fixture. <paramref name="role"/>
    /// defaults to <c>null</c> (absent), which the
    /// generated record's own doc says to treat as owner; a freshly created
    /// set is the caller's own, so owner is also the truthful answer here.
    public static FfiFolder MakeFolder(
        long id = 0, string name = "", string? retentionPolicy = null,
        long cachedSnapshotCount = 0, long cachedTotalBytes = 0,
        long? cachedLastSnapshotAt = null, string[]? includePaths = null, string[]? excludePaths = null,
        string? role = null) =>
        new FfiFolder(
            id, name, retentionPolicy, cachedSnapshotCount, cachedTotalBytes,
            cachedLastSnapshotAt, includePaths, excludePaths, role);

    /// A <c>fauna.search.query</c> result-row fixture.
    public static FfiSearchResult MakeSearchResult(
        string contentType = "post", string contentId = "r0", long createdAt = 0, long rank = 0,
        string snippet = "") =>
        new FfiSearchResult(contentType, contentId, createdAt, rank, snippet);

    // ── fixture helpers (subscriptions / payments) ────────────────────────

    /// A <c>fauna.subscriptions.requests.approve</c> reply fixture.
    public static FfiApproveReply MakeApproveReply(
        byte[]? subscriber = null, string tier = "", ulong keyVersion = 1) =>
        new FfiApproveReply(subscriber ?? new byte[32], tier, keyVersion);

    /// A <c>fauna.subscriptions.reconcile_once</c> pass fixture.
    public static FfiReconcilePass MakeReconcilePass(
        uint resumed = 0, uint approved = 0, string? resumeError = null, string? drainError = null) =>
        new FfiReconcilePass(resumed, approved, resumeError, drainError);

#if PAYMENTS
    /// A <c>fauna.claims.redeem</c> reply fixture (also this mock's
    /// <see cref="NextClaimRedeemReply"/> field default).
    public static FfiClaimRedeemReply MakeClaimRedeemReply(
        byte[]? author = null, string tier = "tier", ulong? validUntil = null, bool queued = true) =>
        new FfiClaimRedeemReply(author ?? new byte[32], tier, validUntil, queued);

    /// A <c>fauna.claims.mint</c> reply fixture (also this mock's
    /// <see cref="NextClaimMintReply"/> field default).
    public static FfiClaimMintReply MakeClaimMintReply(
        string code = "code", string tier = "tier", ulong? validUntil = null) =>
        new FfiClaimMintReply(code, tier, validUntil);
#endif   // PAYMENTS

    // ── fixture helpers (seed rotation / offline share) ────────────────────

    /// A seed-rotation-roster inheritor fixture (also this mock's
    /// <see cref="NextSeedRotateRoster"/> field default).
    public static FfiSeedRotationInheritor MakeSeedRotationInheritor(
        byte[]? actorId = null, string label = "admin@example") =>
        new FfiSeedRotationInheritor(actorId ?? new byte[32], label);

#if P2P_SHARE
    /// An <c>offline_share_load_group_shares</c> views fixture (also this
    /// mock's <see cref="NextGroupShareViews"/> field default).
    public static FfiGroupShareViews MakeGroupShareViews(
        FfiPendingGroupShare[]? invitations = null, FfiGroupScope[]? scopes = null) =>
        new FfiGroupShareViews(
            invitations ?? Array.Empty<FfiPendingGroupShare>(),
            scopes ?? Array.Empty<FfiGroupScope>());
#endif

    // ── fixture helpers (post-succession member review) ────────────────────

    /// A <c>member_reviews_list</c> roster entry fixture.
    public static FfiMemberReview MakeMemberReview(
        byte[]? person = null, string[]? reasons = null) =>
        new FfiMemberReview(person ?? new byte[32], reasons ?? new[] { "compromise_window" });
}
