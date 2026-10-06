using FaunaApp.Core.Models;
using uniffi.fauna_ffi;
using uniffi.fauna_conversations;
using uniffi.fauna_feed;
using uniffi.fauna_devices_machine;
using uniffi.fauna_labeler_catalog_machine;
using uniffi.fauna_atproto_settings_machine;
using uniffi.fauna_media_machine;
using uniffi.fauna_folders_machine;
using uniffi.fauna_backups_machine;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_client_connected_apps;
using uniffi.fauna_client_dns;
using uniffi.fauna_client_pair;
using uniffi.fauna_client_config;
using uniffi.fauna_log;
using uniffi.fauna_client_capabilities;

namespace FaunaApp.Core.Services;

/// <summary>
/// WS-RPC request/reply plane to the nest (the <c>fauna.*.*</c> kinds), over the
/// per-actor WebSocket. The seam the view models depend on so they can be unit
/// tested against a fake; the production implementation is <see cref="NestRpcClient"/>
/// (wrapping the shared UniFFI <c>FfiNestClient</c>), the test double is
/// <c>MockNestRpcClient</c>. Mirrors the <see cref="INestHttpClient"/> /
/// <c>MockNestHttpClient</c> split for the HTTP plane.
/// <para><c>internal</c> because its surface returns UniFFI-<c>internal</c> reply
/// records; the test + WinUI assemblies see it via <c>[InternalsVisibleTo]</c>.</para>
/// </summary>
internal interface INestRpcClient
{
    // ── connection info ─────────────────────────────────────────────────

    /// <summary>
    /// This client's own home nest base URL (the URL <see cref="NestRpcClient"/>
    /// was constructed against). The anonymous find-user-by-handle discovery path
    /// passes it as the <c>home_url</c> argument of
    /// <c>FaunaFfiMethods.ResolveNest</c> — the home nest performs the SRV lookup
    /// that maps a handle's domain to its owning node URL (contacts.md § Where
    /// logic lives → Contact lookup by handle; the same own-home-nest URL linux's
    /// <c>FaunaClient::node_url</c> / android's <c>ApiClient.nodeUrl</c> feed to
    /// <c>resolve_nest</c>).
    /// </summary>
    string HomeUrl { get; }

    // ── reconnect re-hydrate ────────────────────────────────────────────

    /// <summary>
    /// Raised on the UI thread on each WS reconnect — a <c>Connected</c>
    /// transition AFTER the first connect (the initial connect does NOT fire it,
    /// the app loads its surfaces then anyway). Surface view models subscribe
    /// (via <c>ViewModelBase.RefreshOnReconnect</c>) and re-fetch their visible
    /// snapshot — the <c>transport.md</c> § Push events "application observers
    /// re-pull through their snapshot-refresh path on reconnect" contract; the
    /// feed has no poll backstop, so a post that arrived while disconnected would
    /// otherwise stay invisible until a manual refresh. Production raises it from
    /// the reconnect pump over the shared <c>FfiNestClient.SubscribeReconnects</c>
    /// watch (<see cref="NestRpcClient.StartReconnectPump"/>); the test double
    /// raises it via <c>RaiseReconnected()</c>.
    /// </summary>
    event Action? Reconnected;

    /// <summary>
    /// Raised on the UI thread for each inbound contact-request (knock) push
    /// (<c>fauna.knock</c>), carrying the decoded <see cref="FfiKnock"/>. Re-homes
    /// the Windows knock OS toast + the contacts roster refresh onto WS-RPC push —
    /// the dead <c>WebSocketService</c> used to feed these. <c>App</c> subscribes
    /// to fire <c>NotificationService.ShowKnockNotification</c>; <c>ContactsPage</c>
    /// subscribes to refresh its roster while open. <c>conversations.md</c> § Where
    /// logic lives (OS notifications = client glue). Production raises it from the
    /// knock pump over the shared <c>FfiNestClient.SubscribeKnocks</c> stream
    /// (<see cref="NestRpcClient.StartKnockPump"/>); the test double raises it via
    /// <c>RaiseKnock(knock)</c>.
    /// </summary>
    event Action<FfiKnock>? KnockReceived;

    /// <summary>
    /// Raised on the UI thread for each inbound <c>fauna.calendar.changed</c>
    /// push (a durable write landed in one of the actor's calendars — Fauna-
    /// client or external-MUA-via-MDA; put/delete/provision), carrying the
    /// actor id + calendar id (hex) the nest reported — currently unused by the
    /// sole subscriber, which does a blanket re-fetch, but carried for a future
    /// scoped refresh. <c>EventsPage</c> subscribes to re-run its ~10 s poll's
    /// refresh+rebuild path immediately instead of waiting for the next tick —
    /// the poll itself remains the backstop (transport.md § Push events,
    /// ratified 2026-07-17). <c>EventsPage</c> ALSO subscribes to
    /// <see cref="Reconnected"/> with the same handler (closed 2026-09-06,
    /// transport.md § Which surfaces a push invalidates, windows-leg audit) —
    /// it used to sit outside the reconnect sweep entirely, so an ordinary
    /// reconnect or a <c>ResyncRequired</c> push never recovered a dropped
    /// calendar change until the next poll tick. Production
    /// raises it from the push pump over the shared
    /// <c>FfiNestClient.SubscribePushes</c> stream
    /// (<see cref="NestRpcClient.StartPushPump"/>); the test double raises it
    /// via <c>RaiseCalendarPushChanged(actorId, calendarId)</c>.
    /// </summary>
    event Action<string, string>? CalendarPushChanged;

    /// <summary>
    /// Raised on the UI thread for each inbound <c>fauna.addressbook.changed</c> push
    /// (a durable card or book write landed in one of the actor's address books —
    /// Fauna-client or external-CardDAV-MUA-via-MDA), carrying the actor id +
    /// addressbook id (hex) the nest reported — the carddav twin of
    /// <see cref="CalendarPushChanged"/>, and distinct from the
    /// <c>FfiPushEvent.AddressBookChanged</c> variant name for the same reason.
    /// Raised only when the shared classifier marks the push stale for the address
    /// book (<c>StaleSurfacesForPushEvent(ev).addressBook</c>). <c>ContactsPage</c>
    /// subscribes while it is on screen and re-reads the books + the open book's
    /// cards ONLY while its Address Book segment shows (a contacts app's first sync is
    /// one push per card). The Address Book has no poll: the nav-in re-read and the
    /// reconnect are the correctness backstop (transport.md § Push events). Production
    /// raises it from the push pump over the shared
    /// <c>FfiNestClient.SubscribePushes</c> stream
    /// (<see cref="NestRpcClient.StartPushPump"/>); the test double raises it via
    /// <c>RaiseAddressBookPushChanged(actorId, addressbookId)</c>.
    /// </summary>
    event Action<string, string>? AddressBookPushChanged;

    /// <summary>
    /// Raised on the UI thread for each inbound <c>fauna.sync.changed</c> push (a
    /// device recorded a change in a folder this actor participates in —
    /// file-sync.md § Remote-change nudge), carrying the folder NAME the nest
    /// reported. The per-set device-activity roster
    /// (<c>folder-device-activity-item</c>) is a row-detail read outside
    /// <c>DevicesMachine</c>'s snapshot (like the member/actor rosters), so it needs
    /// this own push-driven nudge rather than riding the machine's observer tick;
    /// <c>FoldersPage</c> subscribes to re-fetch <see cref="FoldersDevicesAsync"/>
    /// for the currently-EXPANDED set ONLY (a collapsed or different row is a no-op
    /// — mirrors linux's <c>folder_row_is_expanded</c> guard). Production raises it
    /// from the push pump over the shared <c>FfiNestClient.SubscribePushes</c>
    /// stream (<see cref="NestRpcClient.StartPushPump"/>), alongside the existing
    /// resident-sync-agent nudge (<c>IAgentSyncNudge.PullFolderNowAsync</c>) — both
    /// react to the SAME push, one for the local engine, one for this page's UI; the
    /// test double raises it via <c>RaiseFolderChangedPushed(folder)</c>.
    /// <c>MediaPage</c> also subscribes, for its cross-set aggregate
    /// (<c>fauna.media.list</c>) — any push while it's open means some readable
    /// set changed. Both <c>FoldersPage</c> and <c>MediaPage</c> ALSO subscribe
    /// to <see cref="Reconnected"/> now (closed 2026-09-06, transport.md § Which
    /// surfaces a push invalidates, windows-leg audit) — they used to sit
    /// outside the reconnect sweep entirely, so an ordinary reconnect or a
    /// <c>ResyncRequired</c> push never recovered a dropped sync change.
    /// </summary>
    event Action<string>? FolderChangedPushed;

    // ── connection-state indicator ──────────────────────────────────────

    /// <summary>
    /// Raised on the UI thread for every transport connection-state transition —
    /// the source for the global <c>connection-status</c> indicator (top of the
    /// shell). The first raise carries the <i>current</i> state (so a just-subscribed
    /// consumer paints immediately), then one per transition. <c>transport.md</c>
    /// § Connection-status indicator: this is the <i>visible</i> half of the
    /// reconnect machinery — a transient drop shows live as "Connecting…" and is
    /// deliberately NEVER an error banner / toast. Production raises it from the
    /// connection-state pump over the shared <c>FfiNestClient.SubscribeConnectionState</c>
    /// watch (<see cref="NestRpcClient.StartConnectionStatePump"/>, the twin of the
    /// reconnect pump); the test double raises it via <c>RaiseConnectionState(state)</c>.
    /// Carries the raw <c>FfiConnectionState</c> — the label is resolved through the
    /// single shared owner, <c>connectionStateLabel</c>, not a per-app switch.
    /// </summary>
    event Action<FfiConnectionState>? ConnectionStateChanged;

    /// <summary>
    /// Start the connection-state pump (idempotent). Drives a loop over the shared
    /// <c>FfiNestClient.SubscribeConnectionState</c> watch and raises
    /// <see cref="ConnectionStateChanged"/> on every transition — the first carrying
    /// the current state — marshaled onto the UI thread. The sole consumer
    /// (<c>MainViewModel</c>) subscribes THEN calls this, so the initial state is
    /// never missed. No-op on the test double (tests drive
    /// <c>RaiseConnectionState</c> directly).
    /// </summary>
    void StartConnectionStatePump();

    // ── encrypted CalDAV store (fauna.bridges.* via FfiCaldavClient) ─────
    // The Events page reads/writes the encrypted CalDAV store the mail-bridge
    // MDA serves (events.md § Encrypted store), via the shared FfiCaldavClient
    // (libs/fauna-ffi `FfiNestClient::caldav`). Replaces the legacy plaintext
    // `fauna.calendars.*` / `fauna.events.*` path — calendar ids + event ids are
    // lowercase hex strings throughout (the FFI surface takes hex, not raw bytes).

    /// <summary><c>list_calendars</c> — the caller's CalDAV calendars (the
    /// connection actor is the data scope).</summary>
    Task<IReadOnlyList<FfiCalendarRow>> CaldavListCalendarsAsync();

    /// <summary><c>create_calendar</c> — create a calendar named
    /// <paramref name="name"/>.</summary>
    Task CaldavCreateCalendarAsync(string name);

    /// <summary><c>query_events</c> — the events in calendar
    /// <paramref name="calendarIdHex"/> (hex calendar id). Bodies are decrypted
    /// client-side; the grids window by <c>dtstart</c> in the VM.</summary>
    Task<IReadOnlyList<FfiCalEvent>> CaldavQueryEventsAsync(string calendarIdHex);

    /// <summary><c>query_events_seeded</c> — the CalDAV delta-sync backstop poll's
    /// cost-saving twin of <see cref="CaldavQueryEventsAsync"/> (events.md
    /// § Implementation status today). Returns <c>null</c> when the shared
    /// `fauna_client_caldav::delta_sync::backstop_probe` says calendar
    /// <paramref name="calendarIdHex"/> is unchanged since the last call through
    /// this seam — the caller's already-rendered list is still current and MUST
    /// NOT be touched. Returns the full event list otherwise, identical to
    /// <see cref="CaldavQueryEventsAsync"/>. The seam's sync-tokens live on the
    /// underlying <c>FfiCaldavClient</c> instance, so the implementation must
    /// route every call through the SAME cached instance for a session, never a
    /// fresh one per call (a fresh instance always answers "changed").</summary>
    Task<IReadOnlyList<FfiCalEvent>?> CaldavQueryEventsSeededAsync(string calendarIdHex);

    /// <summary><c>query_invited_events</c> — the events the caller is invited to
    /// across calendars they don't own.</summary>
    Task<IReadOnlyList<FfiCalEvent>> CaldavQueryInvitedEventsAsync();

    /// <summary><c>get_event</c> — full detail for event
    /// <paramref name="uidHashHex"/> (hex uid_hash), or <c>null</c> if absent.</summary>
    Task<FfiCalEvent?> CaldavGetEventAsync(string uidHashHex);

    /// <summary><c>create_event</c> — create an event in calendar
    /// <paramref name="calendarIdHex"/>. <paramref name="location"/>/
    /// <paramref name="description"/> are mapped to <c>""</c> when null (the FFI
    /// wants non-null strings; <c>""</c> omits the field).</summary>
    Task CaldavCreateEventAsync(string calendarIdHex, string summary, string dtstart, string dtend, string? location, string? description);

    /// <summary><c>delete_event</c> — cancel event <paramref name="uidHashHex"/>.</summary>
    Task CaldavDeleteEventAsync(string uidHashHex);

    /// <summary><c>rsvp_event</c> — RSVP to event <paramref name="uidHashHex"/>.
    /// <paramref name="response"/> is the typed <c>going</c>/<c>interested</c>/
    /// <c>declined</c> submission set — <c>tentative</c> is inbound-only and has
    /// no case here (caldav-server.md § RSVP semantics).</summary>
    Task CaldavRsvpEventAsync(string uidHashHex, uniffi.fauna_core.RsvpResponse response);

    /// <summary><c>set_reminder</c> — set the caller's reminder
    /// <paramref name="offset"/> (ICS duration) for event <paramref name="uidHashHex"/>;
    /// <c>""</c> clears it.</summary>
    Task CaldavSetReminderAsync(string uidHashHex, string offset);

    /// <summary><c>invite_attendee</c> — invite <paramref name="email"/> to event
    /// <paramref name="uidHashHex"/> (<c>""</c> re-sends to the existing roster).</summary>
    Task CaldavInviteAttendeeAsync(string uidHashHex, string email);

    // ── fauna.conversations.keypackage.* ────────────────────────────────
    // The MLS key-package POOL only (count + upload). All MLS ops run in shared
    // Rust (conversations.md rule #2) — these wrap the FfiConversationsClient seam.
    // The conversation send/receive rails + cross-nest keypackage.fetch are the
    // deferred FaunaMls rails slice, not here.

    /// <summary><c>fauna.conversations.keypackage.count</c> — remaining
    /// non-expired key packages for the connection actor.</summary>
    Task<int> KeypackageCountAsync();

    /// <summary><c>fauna.conversations.keypackage.upload</c> — publish raw MLS
    /// KeyPackage bytes for the connection actor; returns how many were stored.</summary>
    Task<uint> KeypackageUploadAsync(IReadOnlyList<byte[]> packages, bool lastResort = false);

    // ── fauna.posts.* ────────────────────────────────────────────────────
    // The feed-page list/create/delete + posts.create/get reads moved onto the
    // shared FfiFeedManager in the 2026-06 feed-snapshot lift (feed.md § State &
    // data shape). The raw `PostsInteractAsync` passthrough went with the
    // interaction-counts lift: a bare
    // `fauna.posts.interact` DISCARDS the reply carrying the nest's post-act
    // counters, so the tapped count never moved. Every interaction now goes
    // through `FfiFeedManager::interact`/`like`, which folds those counters into
    // the loaded window — do not reintroduce the passthrough. (Web deleted its
    // `interactWithPost` helper in the same change; windows' member outlived it
    // only because that session had no Windows machine to compile the deletion on.)

    /// <summary><c>fauna.posts.get</c> → the post's extracted body text, for a
    /// moderation-queue <b>server-row</b> spam train on the sealed client-write path
    /// (mirrors linux <c>train_moderation_flow</c>: <c>posts_get</c> →
    /// <c>DecodePostFull().body</c>). <c>null</c> on a missing / bodiless / undecodable
    /// post, so the caller degrades to the server-side <c>fauna.moderation.train</c>.</summary>
    Task<string?> PostBodyTextAsync(string contentId);

    // ── fauna.account.* / fauna.quota.get ───────────────────────────────

    /// <summary><c>fauna.account.get</c> composed into the app's
    /// <see cref="IdentityInfo"/> (actor id + handle from the reply, nest URL
    /// from local config). Returns <c>null</c> when no keypair is loaded;
    /// falls back to local-only identity (actor id, null handle) when the nest
    /// is unreachable — mirroring the old HTTP <c>GetIdentityAsync</c>.</summary>
    Task<IdentityInfo?> GetIdentityAsync();

    /// <summary><c>fauna.quota.get</c> — tier-aware usage breakdown (storage /
    /// inbox / devices / features).</summary>
    Task<FfiQuotaGetReply> QuotaGetAsync();

    /// <summary>The controversial-class feature plane's whole transparency-read
    /// surface, ready to render (<c>FfiFeaturesClient.Rows</c> — joins
    /// <c>fauna.features.status</c> with <c>fauna.nest.info</c>'s capability set
    /// internally, so the caller never needs the capability array itself).
    /// Every judgement (bounds, headroom, per-cell tier attribution,
    /// available/disabled/hidden) is shared-Rust output — this is a render
    /// source, never a fold point (dynamic-features.md § Transparency &amp;
    /// auditability).</summary>
    Task<IReadOnlyList<FfiFeatureRow>> FeaturesRowsAsync();

    /// <summary>Ask this session's nest — the relay, <c>fauna.region.artifact.get</c>
    /// — for every policy on the device's declared region chain, and fold the
    /// verified answers into <paramref name="plane"/> (<c>FfiRegionPlane.refresh</c>,
    /// or <c>refresh_if_due</c> on the shared cadence when
    /// <paramref name="onlyIfDue"/>). Returns whether the render must re-read
    /// (region-blocking.md § How an app obtains its region's policy). A failed ask
    /// writes nothing.</summary>
    Task<bool> RefreshRegionPlaneAsync(FfiRegionPlane plane, bool onlyIfDue);

    /// <summary><c>fauna.account.am_i_admin</c> — whether the calling actor is a
    /// nest admin. Drives the gated Admin nav entry (MainPage). Replaces the
    /// dead <c>GET /admin/api/stats</c> HTTP probe; fail-closed (the caller
    /// treats any error as not-admin, keeping the admin UI hidden).</summary>
    Task<bool> AmIAdminAsync();

    /// <summary><c>fauna.dns.set_host_address</c> — report the nest's PUBLIC IP so
    /// ACME HTTP-01 gates on the STRONG resolve-check
    /// (<c>domains-and-tls-bootstrap.md</c> § Host-address acquisition). A thin
    /// pass-through to the shared <c>report_host_address</c> FFI fn (the native
    /// twin of linux's direct drive / web's <c>reportHostAddress</c>): it
    /// classifies the dial-address, NEVER publishes a private/LAN one, and reports
    /// the public IP when determinable — all client logic lives in the shared fn
    /// (priority #2). The caller admin-gates (a non-admin's call is refused
    /// nest-side → <see cref="FfiHostAddressOutcome.Failed"/>). Idempotent
    /// (last-writer-wins on the nest); the outcome is for the log line only.</summary>
    Task<FfiHostAddressOutcome> ReportHostAddressAsync();

    /// <summary><c>fauna.account.delete</c> — queue account deletion as a
    /// pending action with a cancellation window.</summary>
    Task AccountDeleteAsync();

    /// <summary><c>fauna.profile.handle.change</c> — queue a handle change as a
    /// pending action (delayed + cancellable). The authenticated bearer kind the
    /// Settings → Account "change handle" affordance rides; it replaced the old
    /// abuse of the anonymous <c>register</c> route for handle changes (matching
    /// linux <c>client.change_handle</c> / apple <c>api.changeHandle</c>).</summary>
    Task ChangeHandleAsync(string handle);

    /// <summary><c>fauna.pending_actions.list</c> — this actor's queued
    /// destructive operations, narrowed to still-<c>pending</c> rows (the wire
    /// reply carries every status; mirrors tui's/linux's/web's/android's own
    /// client-side filter — <c>settings.md</c> § Pending actions).</summary>
    Task<IReadOnlyList<FfiPendingActionSummary>> PendingActionsListAsync();

    /// <summary><c>fauna.pending_actions.cancel</c> — cancel a scheduled
    /// action before it executes (one click, no confirm — cancelling is the
    /// safe direction).</summary>
    Task PendingActionCancelAsync(long id);

    // ── fauna.bridges.* ─────────────────────────────────────────────────

    /// <summary><c>fauna.bridges.list</c> — the bridge roster, each mapped into
    /// the app's <see cref="BridgeInfo"/> (the structured <c>identity</c> reply
    /// collapses to its display string; <c>linked</c> drives the active count on
    /// the status page).</summary>
    Task<IReadOnlyList<BridgeInfo>> BridgesListAsync();

    /// <summary><c>fauna.bridges.link</c> (<c>forbid_replay</c>, 30 s). The
    /// per-mode field values (<paramref name="fields"/>) are composed into the
    /// <c>params</c> CBOR map here; <paramref name="mode"/> is the link mode
    /// (<c>oauth</c> / <c>smtp</c> / <c>nsec</c> / …) matching the linux dialogs.
    /// The <c>LinkReply</c> is discarded — the UI reloads the roster.</summary>
    Task BridgesLinkAsync(string bridgeId, string mode, IReadOnlyDictionary<string, string> fields);

    /// <summary><c>fauna.bridges.unlink</c>.</summary>
    Task BridgesUnlinkAsync(string bridgeId);

    /// <summary><c>fauna.bridges.set_settings</c> — write the named provider
    /// settings. The map is PARTIAL (the nest's wire type is all-optional), so
    /// pass only the keys being changed. Backs the Nostr page's 5 content
    /// toggles and its <c>relay_list</c> read-modify-write (nostr.md § User
    /// actions).</summary>
    Task BridgesSetSettingsAsync(string bridgeId, IReadOnlyList<BridgeSettingValue> settings);

    /// <summary><c>fauna.bridges.list_follows</c> — the per-bridge follow roster.</summary>
    Task<IReadOnlyList<BridgeFollow>> BridgesListFollowsAsync(string bridgeId);

    /// <summary><c>fauna.bridges.add_follow</c> (<c>forbid_replay</c>).</summary>
    Task BridgesAddFollowAsync(string bridgeId, string id, string? petname);

    /// <summary><c>fauna.bridges.remove_follow</c>.</summary>
    Task BridgesRemoveFollowAsync(string bridgeId, string followId);

    /// <summary><c>fauna.bridges.feeds.list</c> — the bridge-feed subscriptions.</summary>
    Task<IReadOnlyList<BridgeFeedSubscription>> BridgesFeedsListAsync();

    /// <summary><c>fauna.bridges.feeds.create</c> — subscribe to a bridge feed.
    /// The new row id is discarded; the UI reloads the list.</summary>
    Task BridgesFeedsCreateAsync(string bridge, string feedUri, string name);

    /// <summary><c>fauna.bridges.feeds.delete</c>.</summary>
    Task BridgesFeedsDeleteAsync(long id);

    // ── fauna.nostr.bunker.* ────────────────────────────────────────────
    //
    // The Nostr page's connect-invite start (nostr.md § The nest as the user's
    // NIP-46 signer). A separate block from `fauna.bridges.*` above because it
    // deliberately is one: "a roster with mint/revoke verbs, not a settings
    // blob" (§ Control plane). User-class, caller-scoped. The roster and the
    // revoke verb are the Connected apps page's (its machine reads and revokes
    // signer rows), so this seam carries only the mint.

    /// <summary><c>fauna.nostr.bunker.create_invite</c> — mint a pending
    /// connection. The reply is the <b>one-time</b> reveal of the
    /// <c>bunker://…</c> connect string; the embedded secret is single-use with
    /// a hard-coded TTL and is never retrievable again.</summary>
    Task<BunkerInvite> NostrBunkerCreateInviteAsync();

    /// <summary>The succession-aftermath npub-confirm banner's nav-enter read
    /// (<c>docs/goal/ui/nostr.md</c> § Key succession and rotation, leg 3) —
    /// best-effort like the shared predicate itself: any unhappy read degrades
    /// to false rather than throwing, so a bad read never turns into a page
    /// error and is just re-asked on the next load.</summary>
    Task<bool> NpubConfirmationOwedAsync();

    /// <summary>Record the owner's "yes, that's my npub" confirmation. The
    /// caller re-reads via <see cref="NpubConfirmationOwedAsync"/> afterward —
    /// non-optimistic, like every other mutation on this page.</summary>
    Task ConfirmNpubAsync();

#if PAYMENTS
    // ── fauna.nostr.zap_signers.* ────────────────────────────────────────
    //
    // The Nostr page's *Zap signers* section (monetization.md § Zap receipts —
    // the trust model; nostr.md § Layout & flow item 7) — the NIP-57 trust
    // root a payee designates. `zaps` is a SUBSET member of `payments` and
    // rides the SAME store-safe axis as the Payments plane above (one C#
    // `PAYMENTS` define for the whole family): a store-safe build's generated
    // C# face has no `FfiNostrZapSignerClient`/`FfiZapSignerEntry` at all, so
    // these signatures would not compile there even if the define were left
    // on. Unlike Connected apps just above, this is rendered for ANY linked
    // account (custody-independent) — reference: apple `NostrVM` (the primary
    // UniFFI-consumer leg), tui `nostr.rs::zap_signers_elements` (lead app).

    /// <summary><c>fauna.nostr.zap_signers.list</c> — the caller's designated
    /// signers, newest first. An empty list means this payee believes no zap
    /// receipt at all (the ratified out-of-the-box default, not an error).</summary>
    Task<IReadOnlyList<ZapSignerEntry>> NostrZapSignersListAsync();

    /// <summary><c>fauna.nostr.zap_signers.add</c> — designate a signer
    /// (64-hex pubkey, normalized lowercase nest-side) with an optional
    /// label. Idempotent: re-adding a designated signer refreshes its label.
    /// Returns the stored row — render THAT pubkey, never the typed input.</summary>
    Task<ZapSignerEntry> NostrZapSignersAddAsync(string signerPubkey, string label);

    /// <summary><c>fauna.nostr.zap_signers.remove</c> — stop trusting a
    /// signer, keyed by the pubkey itself. False when the caller had not
    /// designated it. Deliberately ungated (removal is de-escalation).</summary>
    Task<bool> NostrZapSignersRemoveAsync(string signerPubkey);
#endif

    // ── fauna.email.* ───────────────────────────────────────────────────

    /// <summary><c>fauna.email.filters.list</c> — the per-account email filter
    /// rows (returned as the FFI records; the panel maps them to its list).</summary>
    Task<IReadOnlyList<FfiEmailFilter>> EmailFiltersListAsync();

    /// <summary><c>fauna.email.filters.create</c>. The caller composes the typed
    /// <paramref name="rules"/> / <paramref name="action"/> from the dialog (the
    /// dropdown-tag → variant mapping is UI knowledge). The new row id is
    /// discarded; the panel reloads.</summary>
    Task EmailFiltersCreateAsync(
        string name, IReadOnlyList<FfiEmailFilterRule> rules,
        string combination, FfiEmailFilterAction action, int priority);

    /// <summary><c>fauna.email.filters.delete</c>.</summary>
    Task EmailFiltersDeleteAsync(long id);

    /// <summary><c>fauna.email.filters.get</c> — a fresh single-row fetch, used
    /// by the edit dialog rather than the cached list row.</summary>
    Task<FfiEmailFilter> EmailFiltersGetAsync(long id);

    /// <summary><c>fauna.email.filters.update</c> — overwrite an existing filter
    /// row in place (the edit dialog's save). Same shape as
    /// <see cref="EmailFiltersCreateAsync"/> plus the target <paramref name="id"/>.</summary>
    Task EmailFiltersUpdateAsync(
        long id, string name, IReadOnlyList<FfiEmailFilterRule> rules,
        string combination, FfiEmailFilterAction action, int priority);

    // ── fauna.knocks.* / fauna.contacts.* / fauna.inbox.mode.* ──────────

    /// <summary><c>fauna.knocks.list</c> — the calling actor's pending inbound
    /// knocks, mapped to the app's <see cref="KnockInfo"/> (the FFI
    /// <c>sender</c> hex becomes <see cref="KnockInfo.ActorId"/> — the
    /// accept/block/dismiss key; <c>Handle</c> is null, the list carries none).</summary>
    Task<IReadOnlyList<KnockInfo>> KnocksListAsync();

    /// <summary><c>fauna.knocks.accept</c> — accept the knock from
    /// <paramref name="peerId"/> (hex).</summary>
    Task KnocksAcceptAsync(string peerId);

    /// <summary><c>fauna.knocks.block</c> — block the knock sender
    /// <paramref name="peerId"/> (hex).</summary>
    Task KnocksBlockAsync(string peerId);

    /// <summary><c>fauna.knocks.unblock</c> — unblock <paramref name="peerId"/>
    /// (hex), the guarded clear-the-edge (<c>ContactStatus</c> → <c>None</c>; a
    /// no-op on a non-<c>blocked</c> edge — <c>contacts.md</c> § Where logic lives
    /// → Unblock). The inverse of <see cref="KnocksBlockAsync"/>; drives the
    /// <c>profile-block-button</c> Block⇄Unblock toggle's unblock direction.</summary>
    Task KnocksUnblockAsync(string peerId);

    /// <summary><c>fauna.knocks.dismiss</c> — dismiss the knock from
    /// <paramref name="peerId"/> (hex).</summary>
    Task KnocksDismissAsync(string peerId);

    /// <summary><c>fauna.contacts.list</c> — the calling actor's contact roster,
    /// mapped to <see cref="ContactInfo"/> (FFI <c>peer_id</c>→<c>ActorId</c>;
    /// status maps 1:1 to <see cref="ContactStatus"/>, including the distinct
    /// <see cref="ContactStatus.Confirmed"/>; <c>Handle</c> is null — the roster
    /// carries no handle). Drives both the contacts list and the find-by-actor-id
    /// affordance.</summary>
    Task<IReadOnlyList<ContactInfo>> ContactsListAsync();

    /// <summary><c>fauna.contacts.confirm</c> — promote the <paramref name="peerId"/>
    /// (hex) edge from <see cref="ContactStatus.Accepted"/> to
    /// <see cref="ContactStatus.Confirmed"/>. Nest-guarded to an already-accepted
    /// edge (contacts.md § Where logic lives → Contact confirm); drives
    /// <c>contact-confirm</c>.</summary>
    Task ContactsConfirmAsync(string peerId);

    /// <summary><c>fauna.inbox.send</c> — send an add-contact knock to
    /// <paramref name="actorId"/> (64-hex). Composes the canonical signed
    /// <c>(ContactRequest, Post)</c> <c>email/v1</c> tuple with the shared writer
    /// (<c>FaunaFfiMethods.BuildSignedEmail</c> — no per-app <c>(CR, Post)</c>
    /// builder, priority #2) and hands it to the home nest, which local-delivers
    /// (same-nest, <c>recipient_nest_url</c> null — faithful to the retired
    /// <c>POST /api/v1/inbox/{actor}</c> twin, which only reached same-nest
    /// recipients). The WS-RPC successor of that twin
    /// (<c>federation.md</c> § Federation residue surface); mirrors linux
    /// <c>client.send_knock</c> / apple <c>sendToInbox</c>.</summary>
    Task SendKnockAsync(string actorId);

    /// <summary><c>fauna.inbox.mode.get</c> — the calling actor's inbox-acceptance
    /// mode (<c>open</c> / <c>allow_knock</c> / <c>contacts_only</c> / <c>closed</c>).</summary>
    Task<string> InboxModeGetAsync();

    /// <summary><c>fauna.inbox.mode.set</c> — set the inbox-acceptance
    /// <paramref name="mode"/>; an unrecognized mode is an invalid-params error.</summary>
    Task InboxModeSetAsync(string mode);

    // ── Post-succession member review (succession-aftermath.md § Propagation
    // → *Removing a flagged member*) — shared FFI free-fns. The owner's open
    // review roster: apps CACHE the list this returns and answer per-row
    // questions from it (a member list paints far more often than
    // the ledger changes) — never a hand-rolled scan.

    /// <summary><c>member_reviews_list</c> — the owner's open review roster
    /// (the succession ledger's open member items). Cache this: the thread-header chip
    /// join (<see cref="uniffi.fauna_ffi.FaunaFfiMethods.MemberReviewMarksForThread"/>,
    /// a synchronous free-fn — no RPC wrapper needed) and the contacts-badge
    /// join both answer from it.</summary>
    Task<IReadOnlyList<FfiMemberReview>> MemberReviewsListAsync();

    /// <summary><c>member_review_keep</c> — the owner recognises
    /// <paramref name="person"/> (raw 32 bytes): every open item for them
    /// closes with no group changes. Returns whether anything was actually
    /// open (a concurrent device may have already answered — a success
    /// no-op, never an error). Callers re-fetch <see cref="MemberReviewsListAsync"/>
    /// afterward — this does not return the fresh roster itself.</summary>
    Task<bool> MemberReviewKeepAsync(byte[] person);

    // ── Post-succession email-filter review (succession-aftermath.md
    // § Adjudicating what the aftermath carries across) — the member plane's
    // twin over the shared `filter_marks_*` FFI free-fns. Callers cache the ids
    // (Helpers.InheritedFilterMarks), never a hand-rolled scan.

    /// <summary><c>filter_marks_list</c> — the ids of every email-filter rule
    /// still awaiting the owner's verdict.</summary>
    Task<IReadOnlyList<long>> FilterMarksListAsync();

    /// <summary><c>filter_mark_keep</c> — the owner recognises rule
    /// <paramref name="filterId"/>; it stays and its mark clears. Returns whether
    /// anything was actually open (a concurrent device may have answered first —
    /// a success no-op, never an error).</summary>
    Task<bool> FilterMarkKeepAsync(long filterId);

    /// <summary><c>filter_mark_removed</c> — record Removed for a rule the
    /// caller's own <see cref="EmailFiltersDeleteAsync"/> has ALREADY deleted.
    /// Records only; there is deliberately no second removal mechanism.</summary>
    Task<bool> FilterMarkRemovedAsync(long filterId);

    /// <summary><c>member_review_remove</c> — evict <paramref name="person"/>
    /// from every group of the owner's they are currently in NOW (re-derived,
    /// never from the stored item), then persist whatever verdict the
    /// eviction EARNED (never one this call chooses). A partial eviction
    /// earns none, so the review item stays open; the returned
    /// <see cref="CrossGroupEviction"/>'s fields are what the caller composes
    /// its own message from (tui's <c>Op::MemberReviewRemove</c> arm is the
    /// reference composition). <paramref name="manager"/> drives the eviction
    /// itself — the SAME <see cref="ConversationsManager"/> instance the
    /// caller's session already holds, never a fresh one.</summary>
    Task<CrossGroupEviction> MemberReviewRemoveAsync(ConversationsManager manager, byte[] person);

    // ── fauna.spam.* (Privacy + Moderation pages) ───────────────────────
    // WS-RPC twins of the deleted `GET|PUT /api/v1/spam/preferences` HTTP
    // (api-layers.md § Moderation & Spam), via the shared FfiSpamClient
    // (libs/fauna-ffi/src/spam.rs). Thresholds are per-mille u16 (0–1000); the
    // UI presents a 0.0–1.0 slider (÷1000).

    /// <summary><c>fauna.spam.get_preferences</c> — the calling actor's spam
    /// classifier preferences (per-mille thresholds).</summary>
    Task<FfiSpamPreferences> SpamGetPreferencesAsync();

    /// <summary><c>fauna.spam.set_preferences</c> — partial update; each null
    /// field is left unchanged. The reply echoes the resulting full preferences.</summary>
    Task<FfiSpamPreferences> SpamSetPreferencesAsync(
        ushort? spamThreshold, ushort? phishingThreshold);

    // ── fauna.moderation.* (Moderation page queue + training) ───────────
    // The standalone Moderation page's queue read + per-item training correction,
    // over the shared FfiModerationClient (FfiNestClient::moderation →
    // fauna_client_moderation::ModerationClient — libs/fauna-ffi/src/moderation_client.rs),
    // the SAME seam linux/apple consume (moderation.md § Where logic lives →
    // "Queue retrieval / Training correction submission — shared Rust → nest").
    // The badge label/icon/colour come from the shared fauna_core::content_category
    // map (FaunaFfiMethods.ContentLabelStyle), the action label from
    // obligation_action_label — never hard-coded per client (moderation.md
    // § Categories). Stats (fauna.moderation.stats) has no FfiModerationClient
    // accessor, so the page's count cards stay zero (a separate, still-nest-side gap).

    /// <summary><c>fauna.moderation.actions</c> — the connection actor's own
    /// flagged/actioned content queue (one <see cref="FfiObligationAction"/> per
    /// row: content ref, category, confidence, the obligation action taken).
    /// Empty when nothing was flagged (not an error).</summary>
    Task<IReadOnlyList<FfiObligationAction>> ModerationActionsAsync();

    /// <summary><c>fauna.moderation.train</c> — submit a training correction for a
    /// queue item. The queue is the caller's own flagged content, so the page only
    /// ever submits the not-spam correction (<paramref name="verdict"/> <c>"ham"</c>,
    /// matching linux/apple); it retrains the caller's Bayesian model.</summary>
    Task ModerationTrainAsync(string contentId, string verdict);

    /// <summary><c>fauna.moderation.report_share.set</c> — set the caller's
    /// distributed report-sharing opt-in (report-sharing.md § Client wire +
    /// transparency surface; default off). Caller-scoped; <paramref name="share"/>
    /// <c>false</c> also withdraws every report the caller contributed (nest-side
    /// opt-out sweep). Returns the state now in effect.</summary>
    Task<bool> ModerationReportShareSetAsync(bool share);

    /// <summary><c>fauna.moderation.report_share.status</c> — the caller's opt-in
    /// state plus the transparency list of what this nest publishes to peers
    /// (<c>published</c> is exactly the federation export view — the transparency
    /// guarantee). Pure read; the list may be empty.</summary>
    Task<FfiReportShareStatus> ModerationReportShareStatusAsync();

    /// <summary><c>fauna.moderation.legal_takedown</c> — the admin's legal-takedown
    /// console dispatch (moderation.md § Legal takedown → <i>Invocation surface</i>;
    /// admin.md § N Nest → <i>Legal takedown console</i>). A compulsory act, NOT a
    /// moderation judgement: it removes <paramref name="contentId"/> from serving
    /// and serves a tombstone in its place, and the author can appeal.
    /// <paramref name="restore"/> overturns an earlier takedown, which is why the
    /// legal reference is required going down and optional coming back up — the
    /// gating is the shared fold's (<c>FaunaFfiMethods.TakedownFormView</c>), never
    /// a local check. Returns the reply's raw status (<c>"taken_down"</c> /
    /// <c>"restored"</c>); the user-facing wording is
    /// <c>FaunaFfiMethods.TakedownVerdict</c>'s, never hand-worded here.</summary>
    Task<string> ModerationLegalTakedownAsync(
        string contentId, bool conversation, string legalReference, bool restore);

    /// <summary><c>fauna.bridges.get_spam_threshold_override</c> — the caller's
    /// per-account spam-folder threshold override, in whole points, or
    /// <c>null</c> when the account follows the admin default
    /// (mail-policy-config.md § Tier 3).</summary>
    Task<uint?> SpamThresholdOverrideGetAsync();

    /// <summary><c>fauna.bridges.set_spam_threshold_override</c> — set (or
    /// clear, with <c>null</c>) the caller's per-account spam-folder threshold
    /// override. <c>0</c> is a real setting (turns automatic Junk filing off for
    /// this account), distinct from <c>null</c> (follow the admin default).
    /// Returns the value now persisted.</summary>
    Task<uint?> SpamThresholdOverrideSetAsync(uint? value);

    // ── fauna.family.* (Family page — family-safety.md § App surface) ──

    /// <summary><c>fauna.family.status</c> — "what are my family relationships?":
    /// <c>supervised_by</c> (guardian side, if any) + my active policy on the
    /// supervised side, and <c>wards</c> on the guardian side. Drives both the
    /// Family page and the global <c>supervised-indicator</c>.
    ///
    /// Every SUCCESSFUL read also rewrites the caller's persisted last-known
    /// supervision snapshot (family-safety.md § Content policy, clause 2 —
    /// "written on every successful status read"). This is windows' ONE
    /// status-read choke point (MainPage's admin/family gate is its only
    /// caller today, but persisting here keeps the clause true for every
    /// future one too, mirroring android's <c>ApiClient.familyStatus</c>). A
    /// failed read throws before the write, which is clause 1 by
    /// construction.</summary>
    Task<FfiFamilyStatus> FamilyStatusAsync();

    /// <summary>The persisted last-known supervision snapshot for the actor
    /// named by <paramref name="actorId"/> — <c>null</c> when there is
    /// nothing to enforce (no snapshot, or the last read said unsupervised).
    /// The restore-at-launch call: run BEFORE the first
    /// <see cref="FamilyStatusAsync"/> read, feeding the same stores its
    /// success path feeds (family-safety.md § Content policy, clause 2).
    /// Pure local read, no nest round trip — never call with the active
    /// registry pointer's actor; always the CALLER's own session actor
    /// (mirrors <see cref="FamilyStatusAsync"/>'s own persist key).</summary>
    FfiSupervisionSnapshot? SupervisionSnapshot(string actorId);

    /// <summary><c>fauna.family.policy.update</c> — replace a ward's reach-policy
    /// document (guardian-only).</summary>
    Task FamilyPolicyUpdateAsync(byte[] supervisedActorId, FfiReachPolicy policy);

    /// <summary><c>fauna.family.notify_report</c> — the ward's coarse batched
    /// Guardian Notify report (category + count, never content, never an id —
    /// family-safety.md § Guardian Notify). Sent by <see
    /// cref="Services.GuardianNotifyCache"/>'s flush tick.</summary>
    Task FamilyNotifyReportAsync(FfiFamilyContentNotice[] entries, int utcOffsetMinutes);

    /// <summary><c>fauna.family.usage_report</c> — the ward's foreground-use
    /// heartbeat (family-safety.md § Screen time): <paramref name="minutes"/>
    /// is the client-aggregated foreground delta since the last successful
    /// report; the reply carries the ward-local day + the day's cross-device
    /// total. Sent by <see cref="Services.ScreenTimeCache"/>'s tick.</summary>
    Task<FfiFamilyUsageReport> FamilyUsageReportAsync(uint minutes, int utcOffsetMinutes);

    /// <summary><c>fauna.family.approvals.list</c> — the guardian's reach-approval
    /// queue across all their wards.</summary>
    Task<IReadOnlyList<FfiFamilyApprovalEntry>> FamilyApprovalsListAsync();

    /// <summary><c>fauna.family.approvals.decide</c> — approve/deny one pending
    /// item. Pass the key the <paramref name="kind"/> names and leave the others
    /// empty; a list entry carries all of them. <c>contact</c> /
    /// <c>contact_request</c> are named by <paramref name="peerActorId"/>, a
    /// <c>mail_hold</c> by <paramref name="messageId"/>, and a
    /// <c>feed_source</c> by the whole <paramref name="bridgeId"/> /
    /// <paramref name="operation"/> / <paramref name="target"/> triple
    /// (<paramref name="target"/> is empty for a <c>link</c>; the label is never
    /// part of the key).</summary>
    Task FamilyApprovalsDecideAsync(byte[] supervisedActorId, string kind, byte[] peerActorId, byte[] messageId, string bridgeId, string operation, string target, string peerAddress, bool approve);

    /// <summary><c>fauna.family.contact.add</c> — pre-approve a contact on the
    /// ward's behalf (guardian-side complement of contact-approval mode).</summary>
    Task FamilyContactAddAsync(byte[] supervisedActorId, byte[] peerActorId);

    /// <summary><c>fauna.family.graduate</c> — supervised → full account, in place
    /// (callable by the guardian or the admin).</summary>
    Task FamilyGraduateAsync(byte[] supervisedActorId);

    /// <summary><c>fauna.family.transfer</c> — propose a new guardian for a ward;
    /// pending until the proposed guardian accepts (family-safety.md § Graduation
    /// &amp; transfer; a self-proposal completes immediately). Callable by the
    /// current guardian or the admin.</summary>
    Task FamilyTransferAsync(byte[] supervisedActorId, byte[] newGuardianActorId);

    /// <summary><c>fauna.family.transfer.accept</c> — consent to a proposal naming
    /// the caller as new guardian; completes the re-point.</summary>
    Task FamilyTransferAcceptAsync(byte[] supervisedActorId);

    /// <summary><c>fauna.family.transfer.decline</c> — refuse a proposal naming
    /// the caller; the link stands.</summary>
    Task FamilyTransferDeclineAsync(byte[] supervisedActorId);

    /// <summary><c>fauna.family.transfer.cancel</c> — withdraw the ward's pending
    /// proposal (guardian or admin).</summary>
    Task FamilyTransferCancelAsync(byte[] supervisedActorId);

    /// <summary><c>fauna.family.device.mark</c> — set/clear one of a ward's
    /// registered devices as the guardian's own enrolled device (guardian-only;
    /// family-safety.md § Full visibility for young children / Slice F). Its own
    /// per-device RPC, never batched behind <see cref="FamilyPolicyUpdateAsync"/> —
    /// the flag is a security promise ("the child cannot remove the guardian's
    /// device"), so the flip lands immediately.</summary>
    Task FamilyDeviceMarkAsync(byte[] supervisedActorId, string deviceId, bool marked);

    // ── fauna.folders.* (Media page) ──────────────────────────────────
    // WS-RPC twins of the deleted `/api/v1/file-sets*` HTTP (api-layers.md
    // § Folders), via the shared FfiFoldersClient
    // (libs/fauna-ffi/src/folders_client.rs). The Devices page drives the full
    // surface through DevicesMachine; the Media page only lists + creates.

    /// <summary><c>fauna.folders.list</c> — the bearer actor's folders with
    /// their cached stat columns (the Media page's collection list).</summary>
    Task<IReadOnlyList<FfiFolder>> FoldersListAsync();

    /// <summary><c>fauna.folders.create</c> — create a folder owned by the
    /// bearer actor; returns the created set. A duplicate name surfaces as
    /// <c>fauna.folders.conflict</c>.</summary>
    Task<FfiFolder> FoldersCreateAsync(string name);

    /// <summary>The S8 D1+D3 seal-backfill sweep (file-sync.md § Sealed names
    /// &amp; paths → Implementation status today; apps row 228) — the ONE
    /// sequencing seam every UniFFI app calls at its post-auth hook instead of
    /// hand-rolling the D1-then-D3-skip-member loop itself
    /// (<c>FfiFoldersClient.run_seal_backfill_sweep</c>). Call once per
    /// identity-connected session start — this connection already wires the
    /// resolver + owner BackupKey, no second derivation. Best-effort by
    /// contract; never throws into the sweep, only into a genuinely unreachable
    /// connection — <see cref="Helpers.SealBackfillSweep"/> wraps even that.</summary>
    Task<FfiSealBackfillSweepReport> RunSealBackfillSweepAsync();

    // ── Cross-user sharing (owner side) — folders.md § Sharing ─────────
    // WS-RPC twins of the FfiFoldersClient read + the FaunaFfiMethods.Folders*
    // author free-fns (libs/fauna-ffi/src/folders_client.rs — the shared
    // FoldersAuthor orchestration), consumed by the Folders page's owner-side
    // "Shared with" section (folder-share-button / folder-member-item /
    // folder-member-remove-button).

    /// <summary><c>fauna.folders.members.list_actors</c> — the cross-user
    /// shared-with roster for an ALREADY-SHARED set (distinct from the device
    /// roster, <c>members_list</c>). Callers must only invoke this when the set's
    /// <c>mls_group_id</c> is non-null — an owner-only set returns
    /// <c>fauna.folders.not_shared</c>, which must not surface as a page
    /// error.</summary>
    Task<IReadOnlyList<FfiFolderActorMember>> FoldersMembersListActorsAsync(string name);

    /// <summary><c>fauna.folders.members.set_access</c> — grant or edit a shared
    /// set's member access + optional byte cap (multi-writer Phase 1;
    /// <c>folder-member-role-select</c> / <c>folder-member-cap-input</c>,
    /// owner-editable in place). **Upserts the WHOLE (access, cap) row** — send
    /// both halves on every edit, or the omitted half is cleared (android's
    /// <c>commit(newAccess, newCapText)</c> is the reference).</summary>
    Task FoldersSetMemberAccessAsync(string name, string actorIdHex, string access, long? byteCap);

    /// <summary><c>fauna.folders.devices</c> — the devices that have recorded sync
    /// activity for this set (the Devices-page per-set device-activity roster), each
    /// with its label + total recorded <c>change_count</c>. The ordinary sync
    /// change signal — distinct from the set's cached snapshot totals, which are
    /// snapshot-only (file-sync.md § Implementation status today).</summary>
    Task<IReadOnlyList<FfiFolderDevice>> FoldersDevicesAsync(string name);

    /// <summary><c>fauna.folders.members.list</c> — the set's DEVICE roster (the
    /// sibling of <see cref="FoldersMembersListActorsAsync"/>'s cross-user one),
    /// each seat carrying its place as its flag triple. Feeds the post-create
    /// device-place editor (<c>folder-place-row</c>).
    /// <para>
    /// ⚠ The projection <c>fauna_devices_machine::place_rows</c> has ALREADY
    /// run, at the FFI boundary — these records are painted as-is, never
    /// re-derived here. Every seat paints its three checkboxes
    /// (<c>folder-place-row[j]</c> is the e2e address).
    /// </para>
    /// <para>
    /// <paramref name="devices"/> is the Devices page snapshot's roster, already
    /// unsealed: it NAMES each seat, because a user-chosen device label rests
    /// sealed and the nest sends it empty.
    /// </para></summary>
    Task<IReadOnlyList<FfiFolderMember>> FoldersPlaceRowsAsync(
        string name, IReadOnlyList<DeviceSummary> devices);

    /// <summary>Share an owner-only folder with one member end-to-end
    /// (<c>FoldersAuthor::share_set</c>): fetch the member's KeyPackage, admit them
    /// to a fresh MLS group, bind the channel on the nest, and deliver the Welcome.
    /// <paramref name="memberNestUrl"/> is <c>null</c> for a same-nest member (the
    /// only case the Folders page's owner-side UI drives today).</summary>
    Task<FfiShareOutcome> FoldersShareAsync(ConversationsSession session, string name, byte[] memberId, string? memberNestUrl, string? access = null);

    /// <summary>Remove a member from a shared set
    /// (<c>FoldersAuthor::remove_member</c>): MLS Remove, rotate the content key,
    /// evict from the nest roster. <paramref name="channelId"/> is the set's derived
    /// <c>ChannelId</c> (<c>FaunaFfiMethods.FolderChannelIdFromGroupId</c> on the
    /// caller's <c>mls_group_id</c>).</summary>
    Task<FfiRemoveOutcome> FoldersRemoveMemberAsync(ConversationsSession session, string name, byte[] channelId, byte[] memberId);

    /// <summary>Flip a set's WebDAV serve state end-to-end
    /// (<c>FoldersAuthor::serve_set</c> via <c>FaunaFfiMethods.FoldersServeSet</c>):
    /// content-key genesis/migration + the nest <c>webdav_enabled</c> flag +
    /// <c>WebdavKeysBlob</c> re-provision (ON), or content-key rotation + blob
    /// re-provision without the set (OFF). folders only
    /// (<c>docs/goal/behavior/webdav-server.md</c> § Independent enablement point
    /// 2; <c>docs/goal/ui/folders.md</c> § Element IDs — <c>folder-webdav-toggle</c>).
    /// <paramref name="mlsGroupIdHex"/> is the set's raw <c>mls_group_id</c> hex
    /// (<c>FolderSummary.mlsGroupId</c>) when shared, <c>null</c> when owner-only.
    /// Returns the number of served sets the re-provisioned blob now carries. A
    /// <c>NoMsek</c> failure (the actor has no mail credential yet) surfaces like
    /// any other failed gesture on this page — the disable-with-hint is a tracked
    /// open design question, not built here.</summary>
    Task<uint> FoldersServeSetAsync(ConversationsSession session, string name, string? mlsGroupIdHex, bool enable);

    /// <summary>Paywall a website-enabled folder to one of the creator's own subscription tiers
    /// end-to-end (<c>FoldersAuthor::paywall_set</c> via
    /// <c>FaunaFfiMethods.FoldersPaywallSet</c>): content-key genesis/re-seal + the
    /// nest <c>web_paywall_tier</c> flag + the web-serve holder's
    /// <c>content.read{folder:set}</c> grant mint, in one composition (the face
    /// discovers the holder itself). The structural sibling of
    /// <see cref="FoldersServeSetAsync"/> on owner rows
    /// (<c>docs/goal/ui/folders.md</c> § Web paywall; behavior authority
    /// <c>docs/goal/behavior/monetization.md</c> § Pillar 2 —
    /// <c>folder-paywall-tier-select</c>).
    /// <paramref name="mlsGroupIdHex"/> is the set's raw <c>mls_group_id</c> hex
    /// (<c>FolderSummary.mlsGroupId</c>) when shared, <c>null</c> when owner-only.
    /// v1 is SET-ONLY: there is no un-paywall counterpart wired here yet (the nest-side
    /// revoke leg exists as <c>folders_unpaywall_set</c> but no client offers a clear
    /// affordance — see folders.md § Web paywall).</summary>
    Task FoldersPaywallSetAsync(ConversationsSession session, string name, string? mlsGroupIdHex, string tier);

    /// <summary>Whether the owner can serve ANY set over WebDAV — the capability that
    /// gates <see cref="FoldersServeSetAsync"/> (<c>FaunaFfiMethods.FoldersCanServeWebdav</c>,
    /// <c>docs/goal/behavior/webdav-server.md</c> § Implementation status → <c>6b-2 c
    /// disable-with-hint</c>). Takes no <see cref="ConversationsSession"/> — unlike the
    /// serve call itself, this only needs the owner's config (mail MSEK presence), so a
    /// page can ask it at render time without the conversations rail being wired.</summary>
    Task<bool> FoldersCanServeWebdavAsync();

    // ── Cross-user sharing (recipient side) — folders.md § Sharing ─────
    // WS-RPC twin of the FaunaFfiMethods.FoldersLeave author free-fn, consumed by
    // the Folders page's recipient-side read-only shared-with-me row
    // (folder-leave-button).

    /// <summary>Voluntarily leave a set shared *with* the caller
    /// (<c>FaunaFfiMethods.FoldersLeave</c>): the self-scoped nest roster-drop
    /// (<c>fauna.folders.leave</c> — drops only the caller's own row, no
    /// <c>ownerSecret</c>) followed by the local <c>MlsEngine::forget_group</c>. Does
    /// NOT rotate the owner's content key — a voluntary leave is not a forward-secrecy
    /// threat (folders.md § Sharing). <paramref name="groupIdHex"/> is the set's raw
    /// hex <c>mls_group_id</c> (the caller's own <c>FolderSummary.mlsGroupId</c>) —
    /// unlike the owner-side remove, no <c>ChannelId</c> derivation is needed here.</summary>
    Task FoldersLeaveAsync(ConversationsSession session, string groupIdHex);

    /// <summary><c>fauna.folders.pending_shares</c> — the recipient's staged,
    /// un-acked <c>folder-pending-share</c> knocks (a stranger's share, staged
    /// unprocessed until the user explicitly accepts or declines it). A peek:
    /// listing never acks a row. Fetched once per page load — no push/poll
    /// mechanism (folders.md § Sharing).</summary>
    Task<IReadOnlyList<FfiPendingShare>> FoldersPendingSharesAsync();

    /// <summary>Accept a staged share (<c>folder-share-accept-button</c>):
    /// resolves the Welcome by <paramref name="inboxId"/>, MLS-joins the group
    /// off the chat rail, then acks the durable row. Bypasses the contact gate —
    /// the user has explicitly accepted.</summary>
    Task FoldersAcceptShareAsync(ConversationsSession session, long inboxId);

    /// <summary>Decline a staged share (<c>folder-share-decline-button</c>): a
    /// bare ack of the durable row — the Welcome is dropped unprocessed, so
    /// declining never joins the group.</summary>
    Task FoldersDeclineShareAsync(long inboxId);

    // ── Following a public folder (folders.md § Following a public folder) ──
    // Both write through the shared composition
    // `fauna_client_folders::follow_ops`, whose FFI façade already does the
    // two-arm error match (NotFound carries the ONE ratified wording; anything
    // else is a transport fault). An app leg is a render plus that match — it
    // must never re-derive the address rules.

    /// <summary>Follow a public folder (<c>folder-follow-confirm</c>), addressed
    /// by the OWNER — a handle or a bare 64-hex actor id, the same superset the
    /// share flow takes — plus the folder's plaintext name.
    /// <para>
    /// ⚠ Pass the raw typed <paramref name="owner"/>: classifying it, the
    /// <c>fauna.actor.by_handle</c> hop, the first public fetch that pins the
    /// stable <c>folder_id</c>, and the folding of the three not-found causes
    /// are all the shared recipe's. Resolving the handle here first would be a
    /// fourth re-derivation of rules that exist once on purpose.
    /// </para>
    /// Returns the stored follow list, so the page renders exactly what was
    /// saved rather than splicing a record into a locally-held list.</summary>
    Task<IReadOnlyList<FfiFollowedFolder>> FoldersFollowPublicAsync(string owner, string folderName);

    /// <summary>Unfollow (<c>folder-unfollow-button</c>) — a purely LOCAL
    /// removal: there is nothing to revoke anywhere, because the home nest never
    /// knew this follower existed (the public read plane keeps zero follower
    /// state by design). Idempotent. Returns the stored list.</summary>
    Task<IReadOnlyList<FfiFollowedFolder>> FoldersUnfollowPublicAsync(string homeNestUrl, long folderId);

    /// <summary>Inject the followed-public-folder source into a built
    /// <c>DevicesMachine</c> — the Folders-page twin of
    /// <see cref="WireMediaFollowedFoldersAsync"/>, and the reason
    /// <c>DevicesSnapshot.followed</c> has any rows at all. Best-effort, beside
    /// <see cref="BuildDevicesMachineAsync"/>: unwired, the followed list is
    /// permanently empty and no page error ever says so.</summary>
    Task WireDevicesFollowedFoldersAsync(DevicesMachine devices);

#if P2P_SHARE
    // The ceremony is the `p2p-share` member's other half and an excision unit
    // of its own (dynamic-features.md § Platform-family surface excision): the
    // store-safe fauna_ffi.dll exports no FfiCeremonySeat, CeremonyStatus or
    // FfiGroupShareViews, so a call site outside a P2P_SHARE region is a
    // compile error in that flavor rather than a shipped face.

    // ── Offline co-present share ceremony (Folders page) — p2p.md § Offline
    // share initiation. A nest-free, iroh-direct two-party ceremony; these five
    // calls are the only doors that touch the nest (an admission-floor read)
    // or need a network round-trip at all — everything else (painting the
    // panel, parsing the peer code, resolving status/error text) is pure and
    // called directly off <c>FaunaFfiMethods</c> by the page, matching the
    // existing `IdentityQrEncode` precedent for a locally-held-secret function.

    /// <summary>Bind this session's ceremony seat
    /// (<c>FaunaFfiMethods.OfflineShareBindSeat</c>) — call once, when the
    /// offline-share panel OPENS, never at login: an actor-keyed endpoint for a
    /// feature most people never touch would be waste, and opening the panel is
    /// the co-present user's explicit "I am doing this now" (p2p.md § Offline
    /// share initiation → *Built — the affordance, both roles*). The returned
    /// seat is reused for every subsequent ceremony act while the panel stays
    /// open.</summary>
    Task<FfiCeremonySeat> BindOfflineShareSeatAsync();

    /// <summary>Dial the peer named by <paramref name="peerCodeInput"/> and run
    /// the initiator's half of the ceremony to completion
    /// (<c>FaunaFfiMethods.OfflineShareInitiate</c>).</summary>
    Task<CeremonyStatus> OfflineShareInitiateAsync(FfiCeremonySeat seat, string peerCodeInput);

    /// <summary>Accept a pending group-share invitation
    /// (<c>FaunaFfiMethods.OfflineShareConsent</c>) — one atomic act: mints and
    /// rests the reception keypair, records the accept, awaits the deliver, runs
    /// admission, and writes the machinery through. Requires an already-bound
    /// seat (the invitation was received on it).</summary>
    Task<CeremonyStatus> OfflineShareConsentAsync(FfiCeremonySeat seat, byte[] scopeId);

    /// <summary>Decline a pending group-share invitation
    /// (<c>FaunaFfiMethods.OfflineShareDecline</c>) — a bare ack; nothing is
    /// adopted. Requires an already-bound seat, same as consent.</summary>
    Task OfflineShareDeclineAsync(FfiCeremonySeat seat, byte[] scopeId);

    /// <summary>List pending group-share invitations and already-landed group
    /// scopes (<c>FaunaFfiMethods.OfflineShareLoadGroupShares</c>) — the
    /// consent-card and landed-scope-row data source, read from the group
    /// plane's store, never the ceremony record. <paramref name="seat"/> is
    /// this session's bound ceremony seat, when one is bound: with the nest
    /// unreachable the shared read answers from its in-memory record, so a
    /// co-present consent card still paints (<c>p2p.md</c> § Offline share
    /// initiation).</summary>
    Task<FfiGroupShareViews> OfflineShareLoadGroupSharesAsync(FfiCeremonySeat? seat = null);
#endif

    // ── fauna.search.query (Search page) ────────────────────────────────
    // WS-RPC twin of the deleted `GET /api/v1/search` HTTP (api-layers.md
    // § Search), via the shared FfiSearchClient (libs/fauna-ffi/src/search_client.rs).
    // Caller-scoped (the connection knows its actor). NOTE: a hit's `content_id`
    // is an opaque FTS doc-key (nest-side blake3 of `content_type:natural_id`),
    // NOT a navigable post/actor id — usable for display only, never fed to
    // add-contact / a posts.get.

    /// <summary><c>fauna.search.query</c> — full-text search over the caller's
    /// visible content. <paramref name="contentType"/> filters by schema
    /// (<c>post</c> / <c>imap</c> / <c>profile</c>; null = all). The nest has no
    /// cursor, so the Search page grows <paramref name="limit"/> and re-fetches
    /// the whole page (<paramref name="offset"/> stays 0). Returns the hit rows
    /// (badge + snippet + timestamp; <c>content_id</c> is opaque — see above).</summary>
    Task<IReadOnlyList<FfiSearchResult>> SearchQueryAsync(string query, string? contentType, long? limit, long? offset);

    // ── fauna.notifications.* ───────────────────────────────────────────

    /// <summary><c>fauna.notifications.list</c> — a page of the unified
    /// notification history (raw FFI reply; the VM maps the fields). <paramref name="cursor"/>
    /// is the last-seen id (null ⇒ newest page); <paramref name="limit"/> the page size.</summary>
    Task<FfiNotifListReply> NotificationsListAsync(long? cursor, long? limit);

    /// <summary><c>fauna.notifications.mark_read</c> — mark notifications at or
    /// before <paramref name="upTo"/> (micros; null ⇒ now) read; returns the count flipped.</summary>
    Task<long> NotificationsMarkReadAsync(long? upTo);

    /// <summary><c>fauna.notifications.count</c> — the unread notification count.</summary>
    Task<long> NotificationsCountAsync();

    // ── fauna.setup.status ──────────────────────────────────────────────

    /// <summary><c>fauna.setup.status</c> — the authed setup-wizard progress read.
    /// The <c>admin-nest</c> page reads the reply's serving-port /
    /// router-fronted / host-OS-maintenance fields (admin.md § N Nest).</summary>
    Task<FfiSetupStatus> SetupStatusAsync();

    // ── fauna.admin.* (the user-administration hub; replaces /admin/api/*) ──
    // Mirrors the shared `AdminClient` (libs/fauna-client-admin) over UniFFI's
    // FfiAdminClient. Surface = the admin-users 3-section hub + the admin-settings
    // tier-definition display (docs/goal/behavior/admin.md § Users, § User actions).
    // Tier create/update is deferred (no ui.yaml IDs yet — approval-gated).

    /// <summary><c>fauna.admin.stats</c> — nest-wide counters for the admin
    /// dashboard (users / storage / inbox bytes / WS connections). Replaces the
    /// deleted <c>GET /admin/api/stats</c> HTTP twin.</summary>
    Task<FfiAdminStats> AdminStatsAsync();

    /// <summary><c>fauna.admin.status</c> — the running nest version + any pending
    /// self-update advisory. The admin dashboard's Version card reads it, replacing
    /// the deleted <c>GET /api/v1/node-info</c> HTTP twin.</summary>
    Task<FfiAdminStatus> AdminStatusAsync();

    /// <summary><c>fauna.admin.users.list</c> — paged user roster (<paramref name="offset"/>
    /// + optional <paramref name="limit"/>; page size 50 per admin.md).</summary>
    Task<FfiAdminUsersListReply> AdminUsersListAsync(long? limit, long offset);

    /// <summary>Every account on the nest, newest first — the account list each admin
    /// actor picker offers (admin.md § 2 → <i>Which accounts a picker offers</i>). Pages
    /// <c>fauna.admin.users.list</c> to its total on the nest side
    /// (<c>fauna_client_admin::users_list_all</c>), so no windows call site pages it
    /// itself.</summary>
    Task<FfiAdminUser[]> AdminUsersListAllAsync();

    /// <summary><c>fauna.admin.users.update</c> — change a user's tier (the tier IS the quota).</summary>
    Task AdminUsersUpdateAsync(byte[] actorId, string tier, string label);

    /// <summary><c>fauna.admin.users.evict</c> — evict a user (not deleted; row flips to cancel).</summary>
    Task AdminUsersEvictAsync(byte[] actorId, string reason, string category);

    /// <summary><c>fauna.admin.users.suspend</c> — cut a user off now, no delete
    /// timeline (row flips to the restore control).</summary>
    Task AdminUsersSuspendAsync(byte[] actorId, string reason, string category);

    /// <summary><c>fauna.admin.users.cancel_eviction</c> — cancel a pending eviction
    /// or lift a suspension (both ride the eviction row).</summary>
    Task AdminUsersCancelEvictionAsync(byte[] actorId);

    /// <summary><c>fauna.admin.admins.add</c> — grant the admin role
    /// (<c>admin-users-make-admin-button</c> on a plain, non-admin row).
    /// Schedules an <c>AdminAdd</c> pending action (24h delay, <c>admin.md</c>
    /// § Admin continuity and succession) — the row does not flip to an admin
    /// row right away; a scheduled reply (no error) is success.</summary>
    Task AdminAdminsAddAsync(byte[] actorId);

    /// <summary><c>fauna.admin.admins.remove</c> — revoke the admin role
    /// (<c>admin-users-remove-admin-button</c> on an <c>is_admin</c> row).
    /// Schedules an <c>AdminRemove</c> pending action; refuses
    /// (<c>fauna.admin.conflict</c>) when it would leave zero superadmins —
    /// the nest, not the client, makes that call.</summary>
    Task AdminAdminsRemoveAsync(byte[] actorId);

    /// <summary><c>fauna.admin.users.list</c> filtered to pending evictions (reuses the user row).</summary>
    Task<IReadOnlyList<FfiAdminUser>> AdminEvictionsListAsync();

    /// <summary><c>fauna.admin.tiers.list</c> — tier definitions (caps), for tier pickers + the
    /// admin-settings tier-definition display.</summary>
    Task<IReadOnlyList<FfiAdminTier>> AdminTiersListAsync();

    /// <summary><c>fauna.admin.tiers.update</c> — overwrite the named tier's caps (the
    /// admin-settings in-place tier-cap editing; the name keys the row, caps are raw
    /// i64). A missing tier is <c>fauna.admin.not_found</c>.</summary>
    Task AdminTiersUpdateAsync(
        string name, long maxInboxBytes, long maxStorageBytes, long maxDevices, long maxBlobSize, long maxFeeds);

    // ── fauna.admin.set_registration_mode / fauna.admin.users.create ─────
    // (admin.md § 2 Users → Section 2 Registration, Section 3 Admit;
    // public-mode.md § Registration Modes / § Registration & Identity)

    /// <summary><c>fauna.admin.set_registration_mode</c> — one call carrying both the
    /// registration posture and the orthogonal free-tier ceiling
    /// (<c>admin-users-registration-save-button</c>).</summary>
    Task AdminSetRegistrationModeAsync(FfiRegistrationMode mode, ulong? maxFreeUsers);

    /// <summary><c>fauna.admin.set_age_verification_required</c> — the Registration
    /// section's age require-knob (<c>admin-users-registration-age-verification-toggle</c>,
    /// family-safety.md § The account age band D5+D6), dispatched by the section's one
    /// save beside <see cref="AdminSetRegistrationModeAsync"/>, only when it changed.</summary>
    Task AdminSetAgeVerificationRequiredAsync(bool required);

    /// <summary><c>fauna.admin.users.create</c> — direct admission of a known actor id
    /// (<c>admin-users-admit-button</c>); the third account-creation path. A
    /// <c>null</c> <paramref name="handle"/> admits the deliberate handle-less
    /// state (public-mode.md § A handle-less account).</summary>
    Task AdminUsersCreateAsync(byte[] actorId, string tier, string? handle);

    // ── fauna.admin.membership_tiers.* (monetization.md § Pillar 4) ──────
    //
    // The membership *designation*: a link between one of the admin's OWN
    // subscription tiers (their payee-side creator machinery — read via
    // SubscriptionTiersListAsync) and this page's quota tiers (AdminTiersListAsync).
    // The two tier systems stay distinct concepts joined by this link, never
    // merged (monetization.md:115), so these reads are deliberately separate.

    /// <summary><c>fauna.admin.membership_tiers.list</c> — which of the admin's own
    /// subscription tiers are designated as membership tiers, and the quota tiers an
    /// admitted / lapsed member runs under.</summary>
    Task<IReadOnlyList<FfiAdminMembershipTier>> AdminMembershipTiersListAsync();

    /// <summary><c>fauna.admin.membership_tiers.set</c> — designate or re-point one
    /// subscription tier (an <b>upsert</b>, so re-saving a row re-points the link rather
    /// than conflicting). A <paramref name="tierName"/> the admin does not own is
    /// <c>fauna.admin.not_found</c>; an unknown quota tier is
    /// <c>fauna.admin.invalid_params</c>. An empty <paramref name="lapseTier"/> asks the
    /// nest to apply <c>DEFAULT_LAPSE_TIER</c>.</summary>
    Task AdminMembershipTiersSetAsync(string tierName, string adminTier, string lapseTier);

    /// <summary><c>fauna.admin.membership_tiers.clear</c> — drop one row's designation.
    /// The subscription tier itself is untouched; it reverts to an ordinary content
    /// tier. Designating and clearing never create or destroy either kind of tier.</summary>
    Task AdminMembershipTiersClearAsync(string tierName);

    /// <summary><c>fauna.admin.invite_codes.list</c> — existing closed-registration codes.</summary>
    Task<IReadOnlyList<FfiAdminInviteCode>> AdminInviteCodesListAsync();

    /// <summary><c>fauna.admin.invite_codes.create</c> — mint a code at a tier + max-uses; an
    /// empty <paramref name="code"/> means "nest mints one", returned here (admin.md § 3 mint-on-empty).
    /// <paramref name="guardianActorId"/> links the redeemed account to a guardian for supervised
    /// admission (family-safety.md § Wire &amp; data shape); <c>null</c> mints an ordinary code.
    /// <paramref name="ageBand"/> is the <c>admin-users-invite-age-band-select</c> option VALUE
    /// (the shared <c>age_band_options</c> catalog; its not-set value or <c>null</c> = no band),
    /// meaningful only beside a guardian (family-safety.md § App surface → *Age-band surfaces*).</summary>
    Task<string> AdminInviteCodesCreateAsync(string code, string tier, long uses, byte[]? guardianActorId = null, string? ageBand = null);

    /// <summary><c>fauna.admin.invite_codes.delete</c> — delete an invite code.</summary>
    Task AdminInviteCodesDeleteAsync(string code);

    /// <summary><c>fauna.admin.invite_requests.list</c> — pending onboarding invite requests.</summary>
    Task<IReadOnlyList<FfiAdminInviteRequest>> AdminInviteRequestsListAsync();

    /// <summary><c>fauna.admin.invite_requests.approve</c> — admit the requester at the chosen
    /// <paramref name="tier"/> (creates the account + deletes the request).
    /// <paramref name="guardianActorId"/> links the admitted account to a guardian for supervised
    /// admission (family-safety.md § Wire &amp; data shape); <c>null</c> admits an ordinary account.
    /// <paramref name="ageBand"/> is that row's <c>invite-request-row-age-band-select</c> option
    /// VALUE, meaningful only beside a guardian, as on the mint.</summary>
    Task<FfiAdminInviteRequestApproveReply> AdminInviteRequestsApproveAsync(long id, string? tier, string? label, byte[]? guardianActorId = null, string? ageBand = null);

    /// <summary><c>fauna.admin.invite_requests.deny</c> — deny with an optional reason.</summary>
    Task AdminInviteRequestsDenyAsync(long id, string? reason);

    /// <summary><c>fauna.admin.services.list</c> — the sidecar-service enable flags
    /// (<c>bridge</c> / <c>pairing</c>) backing the <c>admin-services</c>
    /// toggles. The dns toggle is the deployment "Fauna controls DNS" master switch — NOT a
    /// service flag; it rides the shared <c>DnsManagementMachine</c> (admin.md § 5).</summary>
    Task<FfiAdminServiceFlags> AdminServicesListAsync();

    /// <summary><c>fauna.admin.services.update</c> — flip one service flag;
    /// <paramref name="name"/> ∈ {<c>bridge</c>, <c>pairing</c>} (anything
    /// else is <c>fauna.admin.invalid_params</c> nest-side).</summary>
    Task AdminServicesUpdateAsync(string name, bool enabled);

    /// <summary><c>fauna.admin.set_serving_port</c> — set the deployment-wide
    /// client-facing API serving port (the nest's own HTTPS listener: the WS-RPC
    /// transport + the served SPA), an Admin-class node-policy knob mirroring the
    /// CalDAV port. There is no get-RPC — the field hydrates from
    /// <see cref="SetupStatusAsync"/> (<c>FfiSetupStatus.servingPort</c>, default
    /// 443). The nest boot-resolves the <c>serving_port</c> singleton into its own
    /// listener and applies it on the next restart (it cannot hot-rebind its own
    /// listener); inert behind the <c>:443</c> SNI router on a domain box.
    /// <c>nest/common.md</c> § Serving ports.</summary>
    Task SetServingPortAsync(ushort port);

    /// <summary><c>fauna.admin.request_host_restart</c> — the admin "restart now"
    /// affordance on <c>admin-nest</c>: write a <c>restart-requested</c> flag the
    /// host reboot-coordinator consumes on its next run (rebooting regardless of
    /// idle/ceiling, then deleting the flag), via the shared
    /// <c>FfiAdminClient.RequestHostRestart</c>. Rejected
    /// <c>fauna.host_maintenance.no_host</c> on a nest with no maintenance mount
    /// (dev / desktop / bare-metal) — surfaced on the page <c>error-message</c>.
    /// <c>installers/vps.md</c> § Host OS Maintenance § 4.</summary>
    Task RequestHostRestartAsync();

    /// <summary><c>fauna.admin.region.get</c>, folded through the shared
    /// <c>fauna_client_admin::admin_region_view</c> (admin.md § N Nest → Declared
    /// region) — the one call <c>admin-nest-region-section</c> needs to paint every
    /// field. A free FFI function (not <c>FfiAdminClient</c>), same shape as
    /// <c>seed_rotate_roster</c>.</summary>
    Task<FfiAdminRegionView> AdminRegionStatusAsync();

    /// <summary><c>fauna.admin.region.set</c> — declare/re-declare
    /// (<paramref name="region"/> non-null, already validated client-side via
    /// <c>FaunaFfiMethods.AdminParseRegionCode</c>) or withdraw (<paramref
    /// name="region"/> null). Both retire the previous region's feature-policy
    /// document nest-side, so this is never a mere toggle.</summary>
    Task SetRegionAsync(string? region);

    /// <summary>The shared <c>fauna_client_admin::seed_rotation_confirm_view</c>
    /// fold (<c>admin-nest-seed-rotate-button</c>'s arm click,
    /// box-recovery.md § Deployment-seed rotation): the current admin roster —
    /// the set that will inherit the successor deployment identity — plus
    /// whether the destructive confirm may fire at all and why not, when it may
    /// not. A free FFI function over the connected client (not
    /// <c>FfiAdminClient</c>).</summary>
    Task<FfiSeedRotationConfirmView> SeedRotateRosterAsync();

    /// <summary>The ceremony itself (<c>admin-nest-seed-rotate-confirm-button</c>):
    /// mint the successor seed, custody it, dispatch the rotation, and mark the
    /// predecessor rotated. <c>result.rotated</c> is <c>false</c> only for
    /// <c>RefusedIdentityMismatch</c> (the successor seed stays custodied but no
    /// box is serving it). This call
    /// outlives its click: the committed rotation tears down the box's serving
    /// generation, so the app reconnects mid-flight by design.</summary>
    Task<FfiSeedRotationResult> RotateDeploymentSeedAsync();

    /// <summary><c>fauna.oauth.issuer_key_status</c> (<c>admin-nest-oauth-*</c>,
    /// authorization-server.md § The issuer → Two rotation arms) — the section's
    /// one read. A free FFI function over the connected client (not
    /// <c>FfiAdminClient</c>), same shape as <c>AdminRegionStatusAsync</c>.
    /// Non-fatal to the page: the caller folds a thrown exception into the
    /// section's own reason line, never <c>LoadAsync</c>'s <c>Error</c>.</summary>
    Task<FfiIssuerKeyView> AdminIssuerKeyStatusAsync();

    /// <summary><c>admin-nest-oauth-rotate-button</c> — the ordinary rotation:
    /// mints a new signer and retires the outgoing key, which stays accepted
    /// for the retirement horizon. No confirm; the returned sentence IS
    /// <c>admin-nest-oauth-status</c>, success or failure (a reply lost to a
    /// timeout can follow a committed rotation, so the shared fold — never the
    /// caller — decides what to say).</summary>
    Task<uniffi.fauna_core.LocalizedText> AdminRotateIssuerKeyAsync();

    /// <summary><c>admin-nest-oauth-confirm-button</c> — exactly the armed
    /// <paramref name="arm"/>'s kind, worded like
    /// <see cref="AdminRotateIssuerKeyAsync"/>. Disarm before calling.</summary>
    Task<uniffi.fauna_core.LocalizedText> AdminForceRotateIssuerAsync(FfiIssuerForcedArm arm);

    // ── Post-auth passes over the session's ONE connection ──────────────────
    // transport.md: one authenticated WebSocket per actor. Every pass the
    // universal post-auth hook (App.StartMainAppAsync) or the onboarding
    // hand-off fires rides this client's socket; none builds a one-shot
    // FfiNestClient of its own (each such client dialled and minted a bearer of
    // its own, and one session start spent five dials out of the process's
    // per-nest dial budget — transport-connection.md § The dial budget).

    /// <summary>The co-admin deployment-seed custody self-heal
    /// (box-recovery.md § Mechanism, the co-admin bullet) over this session's
    /// connection.</summary>
    Task<FfiDeploymentSeedSelfHeal> SelfHealDeploymentSeedCustodyAsync();

    /// <summary>One critical-alert sweep pass (critical-alerts.md § Mechanism →
    /// <i>Who runs the detector</i>) over this session's connection.</summary>
    Task RunCriticalAlertSweepAsync();

    /// <summary>The critical-alert re-sweep LOOP (critical-alerts.md § Mechanism →
    /// <i>How often the detector runs</i>) over this session's connection; returns
    /// once the identity is torn down.</summary>
    Task RunCriticalAlertSweepLoopAsync();

    /// <summary>The ONE shared post-claim serving-enablement step (onboarding.md
    /// § 3b <i>Mechanism</i>) over this session's connection.
    /// <paramref name="nodeUrl"/> is the nest's literal URL, which derives the MUA
    /// details (never the test-override dial URL).</summary>
    Task ApplyPostClaimServingEnablementAsync(
        string nodeUrl, bool email, bool caldav, bool carddav, bool webdav);

    /// <summary>Register the kit the <c>recovery_kit</c> screen minted and the user
    /// confirmed (identity-succession.md § The RecoveryKey → <i>Creation UX</i>) over
    /// this session's connection.</summary>
    Task RecoveryRegisterDeferredKitAsync(string kitHex);

    /// <summary>A linked-nests machine WITH the trust facet only (no mail relay
    /// hook) — the onboarding one-tap trust mint (onboarding.md § 3b-ter).</summary>
    Task<LinkedNestsMachine> BuildLinkedNestsMachineWithTrustAsync();

    /// <summary><c>fauna.admin.logs</c> — the nest's in-memory <c>fauna-log</c> ring
    /// snapshot (observability.md § Surfaces), as the SAME <see cref="LogEntry"/> the
    /// client's own Settings → Logs page renders. The admin Logs page filters this in
    /// memory (no refetch) and has no clear (no admin RPC to wipe the nest ring).</summary>
    Task<IReadOnlyList<LogEntry>> AdminLogsAsync();

    /// <summary><c>fauna.admin.custody_hosting.list</c> — every hosting row on
    /// this nest, host-attributed (account-data-plane.md § Two-sided bounds). Read class — never desensitizes offline.</summary>
    Task<FfiAdminHostingRow[]> AdminCustodyHostingListAsync();

    /// <summary><c>fauna.admin.custody_hosting.remove</c> — drop one row, keyed
    /// by the <c>(host, grant)</c> pair a <see cref="AdminCustodyHostingListAsync"/>
    /// row carries (never a painted index — a re-read can reorder rows). Also
    /// drops the <c>(host, owner)</c> custodied store when the removed row was
    /// the pair's last. OnlineOnly class (offline_class.rs).</summary>
    Task<FfiAdminHostingRemoveReply> AdminCustodyHostingRemoveAsync(string hostActorId, byte[] grantId);

    // ── fauna.filesync.snapshot.* / fauna.sync.backup_status (Backups page) ──
    // WS-RPC twins of the deleted `/api/v1/snapshots/*` +
    // `/api/v1/sync/backup-status` HTTP routes (web + linux already migrated — the
    // shared `fauna-client-snapshots` / `fauna-client-sync` crates, surfaced via
    // FfiSnapshotsClient / FfiSyncClient). Only the snapshot byte downloads stay
    // HTTP residue (api-layers.md § Snapshots). Replies map to the app Models the
    // Backups page already renders.

    /// <summary><c>fauna.sync.backup_status</c> — the bearer's folders with
    /// their most-recent-change timestamps (the Backups "last backed up"
    /// line).</summary>
    Task<IReadOnlyList<FolderStatus>> BackupStatusAsync();

    // ── fauna.sync.conflicts.* (Sync Conflicts page) ────────────────────
    // WS-RPC twins of the removed `/api/v1/sync/conflicts{,/{id}/resolve}` HTTP
    // routes (deleted nest-side in the WS-RPC cutover — calling them returned an
    // HTML error page the JSON parser choked on). Completes the "B14" conflicts
    // residue the sync section's HTTP twins were lifted ahead of.

    /// <summary><c>fauna.sync.conflicts.list</c> — the bearer's unresolved sync
    /// conflicts, mapped to the <see cref="ConflictInfo"/> the page renders
    /// (candidate versions are dropped; the dedicated Conflicts page resolves
    /// mark-only).</summary>
    Task<IReadOnlyList<ConflictInfo>> ConflictsListAsync();

    /// <summary><c>fauna.filesync.snapshot.list</c> scoped to folder
    /// <paramref name="folder"/> — the Backups snapshot table, newest first.</summary>
    Task<IReadOnlyList<SnapshotInfo>> SnapshotListAsync(string folder);

    /// <summary><c>fauna.filesync.snapshot.get</c> — snapshot metadata + file
    /// listing (the Backups detail pane).</summary>
    Task<SnapshotDetailInfo> SnapshotGetAsync(ulong id);

    /// <summary><c>fauna.filesync.snapshot.create_folder</c> — capture a
    /// point-in-time snapshot of <paramref name="folder"/> (unattributed
    /// client-driven capture). Returns the new snapshot summary.</summary>
    Task<SnapshotInfo> SnapshotCreateFolderAsync(string folder, IReadOnlyList<string> tags);

    /// <summary><c>fauna.filesync.snapshot.delete</c> — queue a 48 h
    /// soft-delete of snapshot <paramref name="id"/>.</summary>
    Task SnapshotDeleteAsync(ulong id);

    /// <summary><c>fauna.filesync.snapshot.prune</c> — apply a retention policy
    /// to <paramref name="folder"/>. Each bucket is <c>null</c> = disabled.</summary>
    Task SnapshotPruneAsync(string folder, uint? keepLast, uint? keepDaily, uint? keepWeekly, uint? keepMonthly);

    // ── Restore surface (backups.md §§ Restore history / divergence / from destination) ──
    // The shared fauna-client-snapshots restore reads + the local restore action,
    // surfaced via FfiSnapshotsClient (no client-side composition — priority #2). The
    // raw FFI reply records cross the seam; the VM maps them to the display rows.

    /// <summary><c>fauna.filesync.snapshot.list_restore_history</c> — the bearer's
    /// <c>restore_history</c> rows, newest first (the <c>restore-history-section</c>).
    /// <paramref name="limit"/> 0 = server default.</summary>
    Task<IReadOnlyList<FfiRestoreHistoryRow>> SnapshotListRestoreHistoryAsync(uint limit = 0);

    /// <summary><c>fauna.filesync.snapshot.list_restore_divergence</c> — the forensic
    /// <c>bridge_restore_divergence</c> rows recorded against snapshot
    /// <paramref name="snapshotId"/>'s restore (the per-row banner + details modal).
    /// Owner-only; empty when the snapshot diverged on nothing.</summary>
    Task<IReadOnlyList<FfiRestoreDivergenceRow>> SnapshotListRestoreDivergenceAsync(long snapshotId);

    /// <summary><c>fauna.filesync.snapshot.restore_message_kind</c> — the local
    /// single-snapshot restore action. <paramref name="confirmId"/> must equal
    /// <paramref name="snapshotId"/> stringified (the friction bar); a mismatch is a
    /// <c>fauna.filesync.snapshot.confirm_mismatch</c> error with no state change. The
    /// reply's <c>config_present == false</c> warns the bridge can't AUTH after restart
    /// until the wrapped-MLS blob bundle is restored too.</summary>
    Task<FfiSnapshotRestoreReply> SnapshotRestoreMessageKindAsync(long snapshotId, string confirmId);

    /// <summary><c>fauna.filesync.snapshot.delete_immediate</c> — owner-only
    /// immediate (skip-soft-delete) removal of snapshot <paramref name="snapshotId"/>.
    /// The friction bar (<paramref name="confirmId"/> == the snapshot id retyped,
    /// <paramref name="acknowledge"/> == the immediate-delete acknowledge string) is
    /// enforced both client-side (the modal pre-gates its confirm button) and by the
    /// nest, which also enforces the hard floor of 3 active snapshots + owner-only.</summary>
    Task SnapshotDeleteImmediateAsync(long snapshotId, string confirmId, string acknowledge);

    /// <summary><c>fauna.filesync.snapshot.list</c> in message-kind mode (folder
    /// null) — the owner-implicit message-kind snapshots feeding the
    /// <c>restore-snapshot-select</c> picker. Distinct from
    /// <see cref="SnapshotListAsync"/> (folder scoped, the Backups table).</summary>
    Task<IReadOnlyList<FfiSnapshotSummary>> MessageKindSnapshotListAsync();

    // ── Backup destinations (management) — shared FFI free-fns ──────────────
    // The cross-location destination CRUD surface (backups.md § Manage backup
    // destinations). Each call is the FFI twin of the wasm
    // WsRpcClient::backupDestination{List,Add,Edit,Remove} — load → resolve
    // (add/edit) → mutate → the plane write (fauna.account.state.put), returning the
    // freshly-persisted list. Pure-consume of the landed
    // libs/fauna-ffi/src/backup_destinations.rs (no client-side logic, priority #2).

    /// <summary><c>backup_destinations_list</c> — the configured backup
    /// destinations from the owner's encrypted <c>fauna.state.backup</c> rows.</summary>
    Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationsListAsync();

    /// <summary><c>backup_destination_add</c> — resolve the candidate's identity
    /// (reachability + authorization), record it, persist. Blank
    /// <paramref name="name"/> defaults to the destination's domain. Returns the
    /// updated list.</summary>
    Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationAddAsync(string url, string name);

    /// <summary><c>backup_destination_edit</c> — rename and/or change the URL of
    /// destination <paramref name="id"/>. A URL change must resolve to the same
    /// nest identity, else the FFI throws <c>FfiException.General</c> with
    /// <c>.msg == "backup-destination-edit-different-nest"</c> (remove + re-add).
    /// Returns the updated list.</summary>
    Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationEditAsync(string id, string url, string name);

    /// <summary><c>backup_destination_enroll_custodian</c> — enroll THIS device as
    /// a client custodian (the second add path, `backups.md` § Third destination
    /// kind → Enrollment). A separate verb rather than <see cref="BackupDestinationAddAsync"/>
    /// with a blank URL: a custodian has no address, so there is nothing to
    /// resolve and no destination nest to open a session with — the three-step
    /// enroll sequence (registry write, config write, no <c>NestBackupKey</c>
    /// grant) mints its own <c>destination_id</c>. <paramref name="custodianDeviceId"/>
    /// must be THIS device's stable sync device id (the same one its file-sync
    /// engines present) — a blank id is refused by the shared enroll, never
    /// defaulted. <paramref name="capacityCapBytes"/> is <c>null</c> for
    /// uncapped, a real choice, never a substituted default. Returns the updated
    /// list.</summary>
    Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationEnrollCustodianAsync(
        string custodianDeviceId, string name, ulong? capacityCapBytes);

    /// <summary><c>backup_destination_remove</c> — drop destination
    /// <paramref name="id"/> (a plain config edit; the coordinator reconciles the
    /// offsite deregistration). Returns the updated list.</summary>
    Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationRemoveAsync(string id);

    /// <summary><c>backup_destination_keep</c> — the owner's <b>Keep</b> on a row
    /// an identity succession carried across (<c>backup-destination-keep-button</c>):
    /// clears that row's post-succession review mark at rest through the shared
    /// CAS path. Returns the re-read list, whose <c>unattested</c> is the at-rest
    /// verdict (<c>succession-aftermath.md</c> § Adjudicating what the aftermath
    /// carries across).</summary>
    Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationKeepAsync(string id);

    // ── Folder destination places (backup-destinations.md § Ordinary-folder
    // coverage) — shared FFI free-fns. The folders page's per-folder
    // *Destination places* section: attach/detach an enrolled backup
    // destination to ONE ordinary folder. Pure-consume of
    // libs/fauna-ffi/src/backup_destinations.rs, the same FFI face android
    // minted (priority #2, no new bindgen owed). Each mutation returns the
    // folder's re-read places — never an optimistic flip, the same posture
    // linux/web/android follow.

    /// <summary><c>folder_destinations_list</c> — this owner's enrolled backup
    /// destinations, each marked attached-or-not for <paramref name="folderId"/>
    /// (<c>fauna.backup.destination.list</c> joined with the sealed config's
    /// display names).</summary>
    Task<IReadOnlyList<FfiFolderDestinationPlace>> FolderDestinationsListAsync(long folderId);

    /// <summary><c>folder_destination_attach</c> — attach <paramref name="folderId"/>
    /// to <paramref name="destinationId"/>, then re-read this folder's places.
    /// Mirrors <see cref="FolderDestinationDetachAsync"/>.</summary>
    Task<IReadOnlyList<FfiFolderDestinationPlace>> FolderDestinationAttachAsync(long folderId, string destinationId);

    /// <summary><c>folder_destination_detach</c> — detach <paramref name="folderId"/>
    /// from <paramref name="destinationId"/>, then re-read this folder's places.
    /// <paramref name="folderSet"/> is the attached row's own
    /// <c>__folder/&lt;hex&gt;/&lt;id&gt;</c> name, carried by the
    /// <see cref="FfiFolderDestinationPlace"/> the detach button's row was built
    /// from — never re-derived here.</summary>
    Task<IReadOnlyList<FfiFolderDestinationPlace>> FolderDestinationDetachAsync(long folderId, string destinationId, string folderSet);

    // ── Muted keywords (moderation.md § Muted keywords) — shared FFI free-fns ──
    // The single user-global tier-1 keyword-mute list
    // (content-moderation-and-ranking.md § Q3). Pure-consume of
    // libs/fauna-ffi/src/muted_keywords.rs (no client-side logic, priority #2);
    // the sealed `fauna.state.moderation` muted-keyword list, nest-opaque.

    /// <summary><c>load_muted_words</c> — the owner's sealed muted-keyword list.</summary>
    Task<MutedWordsSnapshot> MutedKeywordsListAsync();

    /// <summary><c>save_muted_words</c> — replace + persist the owner's
    /// muted-keyword list. Normalized (trim, drop blanks, case-insensitive
    /// dedupe keeping first-seen spelling) by the shared fn; returns the
    /// normalized stored list. ⚠ Whole-list intents only — the page's
    /// add/remove buttons go through the delta pair below.</summary>
    Task<MutedWordsSnapshot> MutedKeywordsSetAsync(IReadOnlyList<uniffi.fauna_core.MutedKeyword> keywords);

    /// <summary><c>add_muted_word</c> — add one term as a DELTA against
    /// the stored list; the shared seam re-reads it inside its own CAS update,
    /// so a concurrent device's term survives.</summary>
    Task<MutedWordsSnapshot> MutedKeywordsAddAsync(string word);

    /// <summary><c>remove_muted_word</c> — the delta pair's inverse;
    /// removing a term already gone is a success no-op.</summary>
    Task<MutedWordsSnapshot> MutedKeywordsRemoveAsync(string word);

    // ── Trained topics (topic-factors.md § Authoring surface & picker) ──
    // The Personalization home's Trained-topics facet: list/create/rename/
    // delete over the shared fauna_client_personalization::topics::TrainedTopics
    // lifecycle (libs/fauna-ffi/src/personalization.rs). No client-side
    // registry↔model-plane sequencing (priority #2) — these are pure
    // pass-throughs, owner secret from session crypto, never the page.

    /// <summary><c>list_trained_topics</c> — every registry entry, each
    /// carrying its advisory example count off the model plane.</summary>
    Task<FfiTrainedTopicRow[]> TrainedTopicsListAsync();

    /// <summary><c>create_trained_topic</c> — mint a trained topic (rejects a
    /// blank name and the registry cap). Returns the fresh row list.</summary>
    Task<FfiTrainedTopicRow[]> TrainedTopicsCreateAsync(string name);

    /// <summary><c>rename_trained_topic</c> — rename in place; the id (and so
    /// the derived composition key) is untouched.</summary>
    Task<FfiTrainedTopicRow[]> TrainedTopicsRenameAsync(byte[] id, string name);

    /// <summary><c>delete_trained_topic</c> — remove the registry entry AND
    /// its paired nest-side model row.</summary>
    Task<FfiTrainedTopicRow[]> TrainedTopicsDeleteAsync(byte[] id);

    /// <summary><c>set_trained_topic_engagement</c> — flip a
    /// trained topic's Layer-A opt-in (<c>learn_from_engagement</c>, the row's
    /// "Learn from my activity" toggle; engagement-cues.md § Layer A).
    /// Registry-only: the model row is untouched, so turning it off stops
    /// future weak training without rewriting what engagement already taught.
    /// Returns the fresh row list.</summary>
    Task<FfiTrainedTopicRow[]> TrainedTopicsSetLearnFromEngagementAsync(byte[] id, bool on);

    // ── Publishing a trained factor as a List (topic-factors.md § Publishing
    // a trained factor; frame D8) ───────────────────────────────────────────
    // The review-prune sheet's two calls: corpus scoring rides the SAME live
    // FfiFeedManager the Feed page built (the corpus is the loaded window —
    // a freshly-built manager would score nothing, mirrors
    // SignalShareStatusAsync's SharedFeedManager() accessor); publishing
    // itself is a stateless free-fn wrapper, owner secret from session
    // crypto, never the page.

    /// <summary><c>FfiFeedManager.score_corpus_for_factor</c> on the SAME live
    /// manager the Feed page built — never a freshly-built one, which would
    /// have no loaded window to score. <paramref name="factor"/> is the
    /// <c>topic:&lt;hex&gt;</c> composition key, and may be ANY trained
    /// factor, not only one the current feed composes. The review bound
    /// (<c>REVIEW_TOP_N</c>) is applied by the shared crate itself, never a
    /// client-supplied N.</summary>
    Task<ScoredExemplar[]> ScoreCorpusForFactorAsync(string factor);

    /// <summary><c>trained_topic_publish_list</c> — publish the review
    /// sheet's pruned exemplar set as a tier-3 List labeler. Owns the entire
    /// lifecycle (derive the per-factor keypair, resolve the next version off
    /// the catalog, build + sign, <c>fauna.labelers.publish</c>) — a thin
    /// pass-through, no client-side re-derivation of any of it.</summary>
    Task<FfiPublishedList> TrainedTopicPublishListAsync(byte[] factorId, string name, IReadOnlyList<FfiPublishEntry> entries);

    // ── Publishing a trained factor as a Model (topic-factors.md § Publishing
    // a trained factor, v2) ─────────────────────────────────────────────────
    // The List twin's shape exactly: corpus rebuild rides the SAME live
    // FfiFeedManager; publishing is a stateless free-fn wrapper.

    /// <summary><c>FfiFeedManager.scrub_corpus_for_factor</c> on the SAME live
    /// manager the Feed page built — rebuilds the factor's publishable
    /// vocabulary from its public, still-fetchable explicit examples. Unlike
    /// <see cref="ScoreCorpusForFactorAsync"/> there is no top-N: the
    /// vocabulary IS the disclosure, so every survivor of the shared prune
    /// floor is returned.</summary>
    Task<TrainedModelReview> ScrubCorpusForFactorAsync(string factor);

    /// <summary><c>trained_topic_publish_model</c> — publish the review
    /// sheet's pruned n-gram set as a tier-3 Model labeler. <paramref
    /// name="moreDocs"/>/<paramref name="lessDocs"/> are the corpus's own
    /// UNSHRUNK counters (the posterior's priors — shrinking them would make
    /// the published model look more confident than it is), never the pruned
    /// entry count.</summary>
    Task<FfiPublishedModel> TrainedTopicPublishModelAsync(byte[] factorId, string name, uint moreDocs, uint lessDocs, IReadOnlyList<FfiPublishNgram> ngrams);

    // The user-global sync preferences (`fauna.state.sync-prefs` — file-sync.md
    // § Conflicts, policy). Pure-consume of libs/fauna-ffi/src/sync_prefs.rs:
    // today the default conflict policy stamped onto NEWLY created folders
    // (the Sync defaults section's sync-default-conflict-policy-select).

    /// <summary><c>load_sync_prefs</c> — the stored default
    /// ("auto" | "latest_wins_always"), or null = no preference (new sets take
    /// the nest column default, auto).</summary>
    Task<string?> DefaultConflictPolicyGetAsync();

    /// <summary><c>save_sync_prefs</c> — set (or clear, with null)
    /// the default; returns the normalized stored value. Existing sets are
    /// untouched (each row's folder-conflict-policy-select stays
    /// authoritative).</summary>
    Task<string?> DefaultConflictPolicySetAsync(string? policy);

    /// <summary><c>backup_destination_status</c> — the per-destination status
    /// (<c>backup-destination-last-upload-time</c> / <c>-backlog-count</c>) for the
    /// configured destinations, one <c>FfiBackupDestinationStatus</c> per destination
    /// keyed by <c>destination_id</c> (backups.md § Per-destination status read).
    /// Repointed 2026-07-24 (slice-4 leg (d)): this is the NEST's
    /// <c>fauna.backup.status</c> projection, read through the shared
    /// <c>fauna_client_config::read_backup_status</c> that every app now calls —
    /// no longer a client-side <c>BackupCoordinator::destination_status()</c>
    /// computation. The <c>deviceId</c> / <c>dataDir</c> parameters are gone with it
    /// (the nest derives the owner from the authenticated connection, and there is no
    /// local coordinator state to path-match), retiring the data_dir canonical-path
    /// contract. Empty when zero destinations are configured.</summary>
    Task<IReadOnlyList<FfiBackupDestinationStatus>> BackupDestinationStatusAsync();

    /// <summary><c>backup_audit_run_pass</c> — run the client-side audit loop's pass
    /// against every configured destination, returning per-destination
    /// <c>FfiDestinationAuditRow</c>s carrying <c>alert_reason</c> (never the raw
    /// verdict — backups.md § Audit-alert surface: the loud/quiet decision cannot be
    /// re-derived on this side of the FFI boundary). <paramref name="statePath"/> is
    /// the caller-resolved, actor-scoped audit-state file
    /// (<see cref="AccountStateDir.BackupAuditStatePath"/>). <paramref name="syncStateDir"/>
    /// is the sync agent's per-actor replica dir
    /// (<see cref="AccountStateDir.SyncAgentStateDir"/>) the covered-folder mirror
    /// plane's population is anchored in; <c>null</c> is the declared absence
    /// (presence over the destination's list alone). The owner secret comes
    /// from session crypto, never the page.</summary>
    Task<IReadOnlyList<FfiDestinationAuditRow>> BackupAuditRunPassAsync(
        string statePath, string? syncStateDir);

    /// <summary><c>backup_audit_observe</c> — feed the audit loop's freshness
    /// comparison an observation: the newest message-kind record this client has
    /// actually rendered, in epoch milliseconds. Purely local (no nest round trip) —
    /// advances <c>AuditStateSnapshot::observed_high_water</c> monotonically in the
    /// same actor-scoped state file <see cref="BackupAuditRunPassAsync"/> reads.
    /// Returns whether the high-water actually advanced.</summary>
    bool BackupAuditObserve(string statePath, long lastActivityMs);

    /// <summary><c>download_snapshot_file_bytes</c> — fetch ONE file's decrypted bytes
    /// out of a snapshot (<c>snapshot-file-download-button[i]</c>, single-file restore;
    /// backups.md § Where logic lives → <i>Single-file byte download</i>). Runs the
    /// shared-Rust client-side walk (<c>fauna_core::file_download</c>: manifest + chunks
    /// by content address → decrypt under the owner key → verify → reassemble), so a
    /// <b>sealed</b> snapshot downloads correctly. The FFI twin of web's
    /// <c>downloadSnapshotFileBytes</c>.
    /// <paramref name="deviceId"/> is the stable sync device id (hex); the owner secret
    /// comes from session crypto here, never from the page. Only the save step that
    /// consumes these bytes is platform glue.</summary>
    Task<byte[]> DownloadSnapshotFileBytesAsync(string deviceId, ulong snapshotId, string path);

    // ── Subscriptions — profile Tiers-tab SELF author management ─────────────
    // The cross-app subscriptions Slice A (monetization.md § Pillar 1 — UX;
    // profile.md). Tier CRUD / request queue / roster reads are thin pass-throughs
    // to the FfiSubscriptionsClient (fauna.subscriptions.* kinds); the three
    // mint-bearing actions (create-tier custody, approve, remove) go through the
    // shared author orchestration free-fns (SubscriptionsAuthor — the encrypted-mode
    // KeyBlob mint+upload lives in shared Rust, priority #2). Pure-consume of the
    // landed libs/fauna-ffi subscriptions surface (apple landed the exposure; no client-side crypto). owner_secret is held inside NestRpcClient.

    /// <summary><c>fauna.subscriptions.tiers.list</c> — the calling author's own
    /// tier definitions (the authenticated own-read powering §1 "My tiers").</summary>
    Task<IReadOnlyList<FfiTierItem>> SubscriptionTiersListAsync();

    /// <summary>Create a tier via the author orchestration
    /// (<c>SubscriptionsAuthor::create_tier</c>): encrypted mode records a fresh
    /// period key in the owner's <c>fauna.state.subscriptions</c> custody then calls
    /// <c>tiers.create</c> (plaintext mints nest-side). <paramref name="askingPriceSats"/>
    /// is the machine-comparable purchase threshold in whole sats
    /// (monetization.md § The asking price); <c>null</c> leaves the tier unbuyable
    /// by an inferring mechanism — the permanently-correct default, not a gap.
    /// Returns whether a row was created.</summary>
    Task<bool> SubscriptionTierCreateAsync(
        string name, uint rank, string? description, string? priceHint, string? paymentUrl, bool autoApprove,
        ulong? askingPriceSats);

    /// <summary><c>fauna.subscriptions.tiers.update</c> — each <c>null</c> field is
    /// left unchanged (name is the server key), including
    /// <paramref name="askingPriceSats"/> (monetization.md § The asking price →
    /// Editability: no client exposes a clear verb yet, so an omitted price KEEPS
    /// the stored one, never clears it). Returns whether a row changed.</summary>
    Task<bool> SubscriptionTierUpdateAsync(
        string name, uint? rank, string? description, string? priceHint, string? paymentUrl, bool? autoApprove,
        ulong? askingPriceSats);

    /// <summary><c>fauna.subscriptions.tiers.delete</c>.</summary>
    Task<bool> SubscriptionTierDeleteAsync(string name);

    /// <summary><c>fauna.subscriptions.requests.list</c> — pending subscribe/
    /// unsubscribe requests for the author's tiers (§2).</summary>
    Task<IReadOnlyList<FfiPendingRequest>> SubscriptionRequestsListAsync();

    /// <summary>Approve a pending request via the author orchestration
    /// (<c>SubscriptionsAuthor::approve_subscriber</c>): mint a roster-covering
    /// KeyBlob under the tier's period key + upload via <c>requests.approve</c>,
    /// retrying roster_mismatch/stale_rotation internally. <paramref name="request"/>
    /// is the row's raw <c>FfiPendingRequest</c> from
    /// <see cref="SubscriptionRequestsListAsync"/>.</summary>
    Task<FfiApproveReply> SubscriptionRequestApproveAsync(FfiPendingRequest request);

    /// <summary><c>fauna.subscriptions.requests.reject</c>.</summary>
    Task<bool> SubscriptionRequestRejectAsync(long requestId);

    /// <summary><c>fauna.subscriptions.subscribers.list</c> — the selected tier's
    /// roster (§3).</summary>
    Task<IReadOnlyList<FfiSubscriberEntry>> SubscriptionSubscribersListAsync(string tierName);

    /// <summary>Remove a subscriber via the author orchestration
    /// (<c>SubscriptionsAuthor::remove_subscriber</c>): rotate to a fresh period
    /// key, mint over the reduced roster, upload via <c>subscribers.remove</c>, then
    /// commit the rotation (crash-staged). <paramref name="subscriberId"/> is the
    /// 32-byte actor id.</summary>
    Task SubscriptionSubscriberRemoveAsync(string tierName, byte[] subscriberId);

    // ── Subscriptions — author-side reconciliation ───────────────────────────
    // The encrypted-mode auto-approve loop (monetization.md § The unifying model
    // grant path 2 + § Pillar 1 → "Where the logic lives"): the nest can't mint in
    // encrypted mode, so a follow (or any auto_approve-tier subscribe) enqueues
    // and the author's client reconciles it. Driven by SubscriptionsAuthorPump;
    // owner_secret held inside NestRpcClient, same as the mint-bearing actions
    // above.

    /// <summary>One author-pump tick — the WHOLE tick body, which is shared policy
    /// (<c>SubscriptionsAuthor::reconcile_once</c>): heal crash-staged subscriber
    /// removals, <em>then</em> auto-approve queued subscribes. Each half is
    /// best-effort and reported in the returned <see cref="FfiReconcilePass"/>;
    /// neither can abort the other, and the call never fails for a bad tick.
    /// <para>There is deliberately no separate resume/drain pair on this seam:
    /// both properties the shared tick guarantees — resume runs <em>before</em>
    /// drain (or the fresh KeyBlob re-covers the subscriber being removed) and
    /// <em>both</em> halves run on <em>every</em> tick (or a removal staged
    /// mid-session heals only at the next connect) — are re-assertable by hand,
    /// and windows got the second one wrong that way. `monetization.md` § Pillar 1
    /// → Where the logic lives: "An app MUST NOT re-derive either."</para></summary>
    Task<FfiReconcilePass> SubscriptionsReconcileOnceAsync();

    // ── Subscriptions — OTHER-profile subscriber browse ──────────────────────
    // The profile Tiers-tab OTHER-profile offers section (monetization.md § Pillar 1
    // surface 2; profile.md § Layout & flow → Another's profile). Thin pass-throughs
    // to the FfiSubscriptionsClient (no client-side crypto — subscriber side): browse
    // another author's offered tiers, read this viewer's status for them, and
    // subscribe (the free "followers" tier is the header follow button).

    /// <summary><c>fauna.subscriptions.offers.list</c> — <paramref name="authorId"/>
    /// (32-byte actor id) is the creator whose offered tier definitions to browse
    /// (the authenticated subscriber-browse read powering
    /// <c>subscription-offers-section</c>). Reuses the <c>tiers.list</c> reply shape.</summary>
    Task<IReadOnlyList<FfiTierItem>> SubscriptionOffersListAsync(byte[] authorId);

    /// <summary><c>fauna.subscriptions.status.get</c> — this viewer's current
    /// subscription status for <paramref name="authorId"/> (the confirmed tier, used
    /// to render each offer row's status badge). The tier is <c>null</c> when not
    /// subscribed.</summary>
    Task<FfiSubscriptionStatus> SubscriptionStatusGetAsync(byte[] authorId);

    /// <summary><c>fauna.subscriptions.subscribe</c> — subscribe this viewer to
    /// <paramref name="tier"/> of <paramref name="authorId"/> (the per-row Subscribe
    /// button, and — with <c>"followers"</c> — the header follow button). Returns
    /// <see cref="FfiSubscribeReply.Approved"/> (plaintext / auto-approve) or
    /// <see cref="FfiSubscribeReply.Queued"/> (encrypted mode — pending the author's
    /// approval/key rotation).</summary>
    Task<FfiSubscribeReply> SubscriptionSubscribeAsync(byte[] authorId, string tier);

    // ── Subscriptions — subscription-settings CONSUMER page ──────────────────
    // Cross-app subscriptions Slice B (monetization.md § Pillar 1 consumer
    // path). Consumer reads own subscriptions (mine.list) and unsubscribes. Thin
    // pass-throughs to FfiSubscriptionsClient; no mint logic (consumer side).

    /// <summary><c>fauna.subscriptions.mine.list</c> — the calling consumer's own
    /// active and pending subscriptions.</summary>
    Task<IReadOnlyList<FfiMineSubscription>> SubscriptionMineListAsync();

    /// <summary><c>fauna.subscriptions.unsubscribe</c> — unsubscribe from the
    /// given author. Returns <see cref="FfiUnsubscribeReply.Removed"/> when the
    /// subscription is immediately removed (plaintext) or
    /// <see cref="FfiUnsubscribeReply.Queued"/> when it enters the pending queue
    /// (encrypted — request waits for the author's key rotation).</summary>
    Task<FfiUnsubscribeReply> SubscriptionUnsubscribeAsync(byte[] authorId);

#if PAYMENTS
    // The whole plane is the App-Store escape hatch's excision unit on windows
    // (dynamic-features.md § Platform-family surface excision). A store-safe build
    // links a fauna_ffi.dll built `--no-default-features --features store-safe`,
    // whose generated C# face has no FfiPaymentsClient and no FfiProviderItem /
    // FfiClaimItem at all — so these signatures would not compile there even if the
    // define were left on. That is the property, not an inconvenience: an un-gated
    // payments call site is a compile error rather than a silent leak.
    // ── Payments — Pillar 3 client legs (monetization.md § Pillars 2+3 — client ──
    // UX; IDs reserved 2026-07-12). Author side: the profile Tiers-tab §4 payment
    // provider section + §5 manual claim-code mint/audit; buyer side: the
    // subscription-settings claim redemption (Pillar 3 Q4's universal fallback
    // binding). Thin pass-throughs to FfiPaymentsClient (fauna.payments.* kinds)
    // — no client-side crypto, the nest owns validation. Pure-consume of the
    // landed libs/fauna-ffi payments surface (linux lead).
    // `payments_known_kinds` / `payments_webhook_url` / `claim_status_label` are
    // pure functions the VM calls directly off FaunaFfiMethods — no round trip.

    /// <summary><c>fauna.payments.providers.list</c> — the calling author's own
    /// configured payment providers, ascending by kind; rows never carry the
    /// webhook secret (§4 <c>subscription-provider-list</c>).</summary>
    Task<IReadOnlyList<FfiProviderItem>> PaymentsProvidersListAsync();

    /// <summary><c>fauna.payments.providers.set</c> — upsert the calling author's
    /// config for one provider <paramref name="kind"/> (webhook-verification
    /// secret + entitled tier). Returns whether the config was saved. Typed
    /// errors: <c>fauna.payments.{unknown_provider,tier_not_found,malformed}</c>.</summary>
    Task<bool> PaymentsProvidersSetAsync(string kind, string webhookSecret, string tier);

    /// <summary><c>fauna.payments.providers.remove</c> — delete the calling
    /// author's config for one provider kind. Idempotent; returns whether a row
    /// was removed.</summary>
    Task<bool> PaymentsProvidersRemoveAsync(string kind);

    /// <summary><c>fauna.payments.claims.redeem</c> — bind a post-payment claim
    /// <paramref name="code"/> to the calling actor; the entitlement lands
    /// through Pillar 1's grant queue. Typed errors:
    /// <c>fauna.payments.claim_{not_found,already_redeemed,voided}</c>.</summary>
    Task<FfiClaimRedeemReply> PaymentsClaimsRedeemAsync(string code);

    /// <summary><c>fauna.payments.claims.mint</c> — the author mints a claim code
    /// manually (§5), for a no-API provider already paid out-of-band. Always
    /// <c>provider = "manual"</c> nest-side; the nest rejects a
    /// <paramref name="tier"/> that isn't one of the author's own.</summary>
    Task<FfiClaimMintReply> PaymentsClaimsMintAsync(string tier, ulong? validUntil);

    /// <summary><c>fauna.payments.claims.list</c> — the calling author's own
    /// claim codes, newest first; the §5 audit surface for BOTH manually-minted
    /// and webhook-minted codes.</summary>
    Task<IReadOnlyList<FfiClaimItem>> PaymentsClaimsListAsync();

#endif   // PAYMENTS

    // ── Profile edit (display-name / bio / links read-modify-write) ──────────
    // The cross-app profile-edit form (profile.md § State & data shape). The
    // read decodes the three editable display fields PLUS hands back the opaque
    // raw body so the save's read-modify-write preserves the non-display fields
    // (avatar / banner / nests / admin_nests / load_hint / inbox_mode). Both the
    // decode and the sign+wire-build live in shared Rust (libs/fauna-ffi
    // build_edited_profile / decode_profile_display — priority #2, no client-side
    // crypto); these are thin FfiProfileClient pass-throughs over the same
    // FfiNestClient. Mirrors linux apps/fauna-linux/src/views/profile/edit.rs.

    /// <summary><c>fauna.profile.get</c> → decode the editable display fields
    /// (<c>decode_profile_display</c>) and return them alongside the opaque raw
    /// body the save needs as its read-modify-write base. Returns <c>null</c> when
    /// the profile is unpublished OR the get/decode fails (treated as unpublished —
    /// the first-publish path; mirrors linux <c>edit.rs</c> / <c>refresh_header_name</c>
    /// falling back to <c>None</c> on a get failure). Used only for the header
    /// refresh (any actor); the edit form's own open reads through
    /// <see cref="LoadProfileEditBaseAsync"/> instead.</summary>
    Task<ProfileGetResult?> ProfileGetAsync(string actorId);

    /// <summary>The profile edit form's base load — this session's OWN stored
    /// profile, read through the shared read-prove-record
    /// (<c>FfiProfileClient.LoadEditBase</c> → <c>load_profile_edit_base</c>)
    /// rather than <see cref="ProfileGetAsync"/>: it proves a succession link the
    /// base needs and records it in the account registry BEFORE the form can save
    /// over it, so a linkless successor's first edit never races the per-sign-in
    /// aftermath hop (profile.md § After an identity succession → the linkless
    /// bullet). Returns <c>null</c> when the profile is unpublished OR the
    /// read/decode fails (treated as unpublished — the first-publish path, mirrors
    /// <see cref="ProfileGetAsync"/> and apple's <c>ProfileEditVM.open</c>).
    /// Mirrors tui/linux/android/apple/web's edit-open leg.</summary>
    Task<ProfileGetResult?> LoadProfileEditBaseAsync();

    /// <summary>Build the signed wire via the shared read-modify-write
    /// (<c>build_edited_profile_with_images</c> — overwrites display-name / bio /
    /// links / avatar / banner, preserving the non-display fields from
    /// <paramref name="baseBody"/>, or first-publish defaults when it is
    /// <c>null</c>) and publish it via <c>fauna.profile.set</c>. <paramref
    /// name="avatar"/>/<paramref name="banner"/> resolve the picture fields —
    /// <c>Keep</c> for a text-only save (profile.md § Where logic lives → Field
    /// ownership). Errors propagate to the VM (which surfaces them via
    /// <c>ShowError</c>).</summary>
    Task ProfileSetAsync(
        ProfileDisplay edited, byte[]? baseBody, FfiProfileImageEdit avatar, FfiProfileImageEdit banner);

    // ── Folder creation wizard (shared FolderWizardMachine) ──────────────

    /// <summary>
    /// Build a shared-Rust <see cref="FolderWizardMachine"/> bound to this
    /// session's connected WS-RPC requester (the Devices page's
    /// <c>folder-add-button</c> opens it). The wizard's <c>submit()</c> issues
    /// <c>fauna.folders.create</c> + <c>fauna.folders.places.set</c> over the
    /// same connection (devices.md § Where logic lives — no wizard logic
    /// client-side). <paramref name="observer"/> ticks on every snapshot change;
    /// <paramref name="availableDevices"/> is the page's current enrollable-device
    /// list (each becomes an unselected seat with the default place flags).
    /// </summary>
    Task<FolderWizardMachine> BuildFolderWizardMachineAsync(
        FolderWizardObserver observer, IReadOnlyList<DeviceOption> availableDevices);

    // ── fauna.conversations.* session (shared ConversationsSession) ─────────

    /// <summary>
    /// Build the shared-Rust <see cref="ConversationsSession"/> bound to this
    /// session's WS-RPC requester, OVER the given <paramref name="manager"/>
    /// (<c>libs/fauna-ffi</c> <c>FfiNestClient::conversations_session_over_manager</c>
    /// → <c>ConversationsSession::from_manager</c>) rather than building a fresh
    /// internal one. The real FaunaMls + SMTP rails register onto the SAME manager
    /// instance the caller already holds (<c>ConversationsManagerHost.Instance</c>),
    /// so nothing is swapped and nothing injected into it beforehand is orphaned —
    /// the windows twin of the fix in <c>testing.md</c> § Cross-app e2e
    /// conventions (convention 10 / the manager-swap defect apple hit first). This
    /// is the only place native windows reaches the MLS send / keypackage /
    /// receive rail — there is no client-side MLS state (<c>conversations.md</c>
    /// § Architectural rules #2). The conversations page renders off
    /// <c>session.Manager()</c> in production; login also drives the
    /// best-effort keypackage replenish off it. Returns <c>null</c> when no
    /// session can be built (test double / no identity).
    /// <paramref name="identityDomain"/> is the handle's own domain
    /// (<c>LaunchSnapshot.identity.domain</c>) — production always passes it;
    /// omitted only by the e2e <c>set_state</c> login path, which has no
    /// <see cref="LaunchMachine"/> and instead resolves the domain through
    /// the same <c>fauna.actor.by_handle</c> door the recipient picker
    /// resolves peers through (conversations.md § State &amp; data shape →
    /// Self-address: live, never baked — never compose a half-resolved
    /// address).
    /// <paramref name="deviceIdHex"/> is this device's stable sync device id, the
    /// <c>index</c>-lease seat shared Rust cannot derive (participants.md
    /// § Coordination primitive → <i>The <c>index</c> kind under the lease</i>:
    /// "a lease holder is a <b>device</b>, and a device id is app-owned state that
    /// the FFI <c>conversations_session</c> factory was never handed"). Supplied by
    /// the caller from <c>ISessionAccount.DeviceId</c> — the same plumbing
    /// <see cref="TaskDelegationListAsync"/> and <c>BackupDestinationStatusAsync</c>
    /// already use. Omitted/null yields no seat: the builder still indexes
    /// everything, it just does not coordinate, and the Task-delegation row reads
    /// Waiting-while-running. Also resolves and forwards the active account's
    /// retired owner <c>BackupKey</c>s off the injected account registry
    /// (<c>sync-agent.md</c> § Credential model → <i>Retired owner keys after an
    /// identity succession</i>), feeding the <c>__mls</c> post-succession
    /// re-seal — empty when there is no registry (unit tests) or the identity
    /// never succeeded, which costs nothing.
    /// </summary>
    Task<ConversationsSession?> BuildConversationsSessionAsync(
        ConversationsManager manager, string? identityDomain = null, string? deviceIdHex = null);

    // ── per-user sync agent (shared SyncAgentProvisioner) ───────────────────

    /// <summary>
    /// Build the shared-Rust sync-agent provisioner
    /// (<c>libs/fauna-ffi</c> <c>FfiNestClient::sync_agent_provisioner</c> →
    /// <c>fauna_client_sync::agent::SyncAgentProvisioner</c>) on this session's
    /// <b>connected</b> client — the one the provisioner's <c>fauna.sync.register</c>
    /// renewal-grant mint rides. Mirrors macOS's <c>APIClient.syncAgentProvisioner</c>
    /// (<c>ensureNestConnected().syncAgentProvisioner(…)</c>).
    ///
    /// <para>⚠ Never build it on a fresh <c>new FfiNestClient(…)</c>: that constructor does
    /// not open the socket, so every request the provisioner makes waits out its kind's
    /// deadline and fails as "the connection to the nest was lost". Until 2026-09-22 this
    /// app did exactly that, and the mint's <c>sync.register</c> never reached the nest — no
    /// named device row, silently (sync-agent-credentials.md § Implementation status
    /// today).</para>
    ///
    /// <para>The identity seed is read from this client's own <c>CryptoService</c> and
    /// consumed in-Rust (grant signing, <c>BackupKey</c> derivation); only the minted
    /// capability ever reaches the agent. The agent resolves every set's content keys from
    /// the account's custody itself (on-demand-files.md § Shared sets on a capability host →
    /// <i>One mechanism</i>), so nothing content-key-shaped is passed here.</para>
    /// </summary>
    Task<FfiSyncAgentProvisioner> BuildSyncAgentProvisionerAsync(
        byte[][] predecessorBackupKeys,
        byte[][] predecessorActorIds,
        string deviceId,
        string deviceLabel,
        FfiAgentSpawner spawner,
        FfiProvisioningBearerSource bearerSource,
        FfiAgentReachabilityObserver? reachabilityObserver);

    /// <summary>
    /// Restore this session's nest from the copy <paramref name="agent"/> holds —
    /// the confirmed <c>backup-destination-reseed-confirm-button</c> action
    /// (<c>backup-destinations.md</c> § Re-seed → <i>Where the ceremony runs</i>:
    /// on a desktop the agent hosts the store, so the agent runs the ceremony).
    /// A pass-through to the shared FFI face
    /// <c>FfiSyncAgentProvisioner::reseed_custodian_store</c>, which waits on the
    /// agent's job and runs the post-ceremony re-enrollment itself — macOS makes
    /// the same call, so nothing about the order lives in C#. It sits on this
    /// seam only because it needs the <b>connected</b> client and the identity
    /// seed, both of which this client owns and the page never holds.
    /// </summary>
    Task<FfiReseedResult> ReseedCustodianStoreAsync(IFfiSyncAgentProvisioner agent, string thisDeviceId);

    // ── fauna.feed.* page (shared FeedManager) ──────────────────────────────

    /// <summary>
    /// Build the shared-Rust <see cref="uniffi.fauna_ffi.FfiFeedManager"/> bound
    /// to this session's connected WS-RPC requester (<c>libs/fauna-ffi</c>
    /// <c>FfiNestClient::feed_manager</c> → <c>libs/fauna-feed::FeedManager</c>).
    /// The manager owns the whole Feed page — the post-list read model
    /// (<c>Snapshot()</c> + the <c>FeedSnapshotObserver</c> reactivity), search
    /// re-query, feed create/delete, compose validation + submit, and bridge
    /// subscribe/unsubscribe — all over the same connection, consuming only
    /// <c>fauna.feed.*</c> / <c>fauna.posts.*</c> / <c>fauna.bridges.feeds.*</c>
    /// kinds (feed.md § State &amp; data shape — no client-side post-list state).
    /// Mirrors <see cref="BuildConversationsSessionAsync"/> / the page-machine
    /// factories: reuses the shared auto-reconnecting client. The page registers a
    /// <c>FeedNotifyObserver</c> and re-renders off <c>Snapshot()</c> on each tick.
    /// </summary>
    Task<uniffi.fauna_ffi.FfiFeedManager> BuildFeedManagerAsync();

    // ── search page (shared SearchManager) ──────────────────────────────────

    /// <summary>
    /// Build the shared-Rust <see cref="uniffi.fauna_ffi.FfiSearchManager"/> bound
    /// to this session's connected WS-RPC requester (<c>libs/fauna-ffi</c>
    /// <c>FfiNestClient::search_manager</c> → <c>libs/fauna-client-search::SearchManager</c>).
    /// The manager owns the whole Search page — query/type-filter/paging state,
    /// the nest-arm fetch, and (once a local index is registered — out of this
    /// leg's scope) the local-arm merge (search.md § State &amp; data shape — no
    /// client-side query/paging state). Mirrors <see cref="BuildFeedManagerAsync"/>;
    /// unlike it, needs no actor secret — searching signs nothing. The page
    /// registers a <c>SearchNotifyObserver</c> and re-renders off <c>Snapshot()</c>
    /// on each tick.
    /// </summary>
    Task<uniffi.fauna_ffi.FfiSearchManager> BuildSearchManagerAsync();

    /// <summary>
    /// Register this login's local sealed-index arm (backend 2) on
    /// <paramref name="manager"/>, so the Search page merges local rows with
    /// the nest's instead of running nest-only
    /// (<c>libs/fauna-ffi</c> <c>FfiNestClient::attach_local_search_index</c> —
    /// content-index.md § Where queries run — per app). Call once after
    /// <see cref="BuildSearchManagerAsync"/>; the UniFFI twin of the two lines
    /// tui runs at its post-auth hook and of apple's/android's
    /// <c>attachLocalIndexIfNeeded</c>.
    ///
    /// Returns whether an arm was registered. <c>false</c> is a **normal
    /// state, not an error** — no conversations session yet, or mail is not
    /// enabled on this actor — and the page renders the same either way (no
    /// local rows). Windows rebuilds a fresh manager on every Search-page
    /// load, so a page that entered before mail was enabled simply retries on
    /// its next load rather than needing a persisted retry flag.
    /// </summary>
    Task<bool> AttachLocalSearchIndexAsync(uniffi.fauna_ffi.FfiSearchManager manager);

    // ── Layer-B signal sharing (engagement-cues.md § Layer B) ───────────────
    // The Personalization home's `personalization-share-signals-toggle` +
    // `signal-share-published-list` transparency pane — the sibling of
    // `ModerationReportShareStatusAsync`/`SetAsync` above, but these two ride
    // the shared FfiFeedManager (not a stateless FfiModerationClient call):
    // the manager caches the opt-in for its own signal producer, so both
    // methods MUST read/write the SAME live manager instance the Feed page
    // observes (mirrors DeleteCueRollupAsync's `FeedViewModel.Current?.Manager`
    // accessor — never a second manager). `HydrateSignalOptin` is deliberately
    // NOT wrapped here: it is producer-internal (primes the manager's cached
    // opt-in for the not-yet-built windows capture shell) and no shipped
    // client's pane calls it either (engagement-cues.md § Implementation
    // status today — linux/android/apple all skip it at the pane layer).

    /// <summary><c>fauna.moderation.signal_share.status</c> — the caller's
    /// Layer-B opt-in plus the nest-wide ≥k transparency export (both
    /// <c>signal:*</c> and <c>report:*</c> aggregates — the export view is
    /// nest-wide, byte-identical to the federation export). Reuses
    /// <see cref="FfiReportShareStatus"/>, the identical shape
    /// <see cref="ModerationReportShareStatusAsync"/> returns.</summary>
    Task<FfiReportShareStatus> SignalShareStatusAsync();

    /// <summary><c>fauna.moderation.signal_share.set</c> via the shared
    /// manager, which already returns the re-read status in one round trip
    /// (unlike <see cref="ModerationReportShareSetAsync"/>'s bare bool — no
    /// separate status re-read needed here). Opting out withdraws this
    /// actor's <c>signal:*</c> rows, which may shrink the published list.</summary>
    Task<FfiReportShareStatus> SetSignalSharingAsync(bool share);

    // ── Page-level Devices machine (shared DevicesMachine) ──────────────────

    /// <summary>
    /// Build the shared-Rust page-level <see cref="DevicesMachine"/> bound to this
    /// session's connected WS-RPC requester. It owns the whole Devices page —
    /// the device / folder / conflict reads (<c>Refresh()</c>), the page write
    /// gestures (<c>RemoveDevice</c> / <c>DeleteFolder</c> / <c>ResolveConflict</c>
    /// / <c>SetFolderPaths</c>), and the embedded folder creation wizard
    /// (<c>OpenWizard()</c> / <c>Wizard()</c> / <c>CloseWizard()</c>) — all over
    /// the same connection, consuming only WS-RPC kinds
    /// (<c>fauna.sync.devices.*</c>, <c>fauna.folders.*</c>,
    /// <c>fauna.sync.conflicts.*</c>) via the shared
    /// <c>fauna-client-{sync,folders}</c> adapters (devices.md § State &amp; data
    /// shape → <i>Broader DevicesSnapshot</i>). <paramref name="observer"/> ticks
    /// on every snapshot change; the page re-renders off <c>Snapshot()</c>.
    /// </summary>
    Task<DevicesMachine> BuildDevicesMachineAsync(DevicesObserver observer);

    /// <summary>
    /// Build the shared-Rust <c>BackupsMachine</c> — the Backups page's SNAPSHOT
    /// half (<c>ui/backups.md</c> § Snapshot-list shape) — bound to the session's
    /// connected WS-RPC requester, over the single UniFFI face
    /// <c>build_backups_machine</c>. It owns the folder selector source
    /// (<c>fauna.folders.list</c>, owner-scoped), the selected set's snapshot
    /// list, every page write gesture (create / delete / immediate-delete /
    /// prune preview+execute / check) and the <c>snapshot-detail-files</c> read,
    /// with label custody wired inside the face — so this page never wires
    /// custody a second time (path-sealing.md § THE CONSUMER-WIRING RULE).
    /// <para>Returned as the generated <see cref="IBackupsMachine"/> interface,
    /// not the concrete class, so <c>BackupsViewModel</c> is unit-testable over a
    /// fake (machine-as-seam, the <c>MailListsViewModel</c> idiom).</para>
    /// <paramref name="deviceIdHex"/> is the shell's stable sync device id, wired
    /// onto the machine so manual snapshots carry row provenance (§ Snapshot-list
    /// shape, <i>Create</i> ruling); empty/unparseable leaves rows unattributed,
    /// which is exactly what the wire's <c>Option</c> means.
    /// The page's DESTINATION and RESTORE halves are unaffected and keep their
    /// existing seams.
    /// </summary>
    Task<IBackupsMachine> BuildBackupsMachineAsync(BackupsObserver observer, string deviceIdHex);

    /// <summary>
    /// Wire <paramref name="devices"/>'s foreign-set (cross-nest) list source —
    /// a set shared from ANOTHER nest has no row in this nest's own list, so the
    /// machine unions in the member's own <c>fauna.state.folder-keys</c> records (written at
    /// share-accept; folders.md § Implementation status today → Foreign-set
    /// (cross-nest) list source). Call once per <see cref="DevicesMachine"/>,
    /// beside <see cref="BuildDevicesMachineAsync"/> — unwired, the list simply
    /// carries no foreign rows (mirrors apple's <c>wireDevicesForeignSets</c>).
    /// Best-effort: swallows the (unreachable once connected) <c>FfiException</c>
    /// rather than surfacing a page error.
    /// </summary>
    Task WireDevicesForeignSetsAsync(DevicesMachine devices);

    /// <summary>
    /// Build the shared-Rust <c>LabelerCatalogMachine</c> (the Personalization
    /// home's subscribed-labelers facet + the Community-labelers catalog page)
    /// bound to this session's connected WS-RPC requester — the same
    /// <see cref="BuildDevicesMachineAsync"/> reuse-the-shared-connection pattern.
    /// It owns the <c>fauna.labelers.{list,inspect,subscribe,unsubscribe}</c>
    /// reads/gestures (<c>libs/fauna-labeler-catalog-machine</c>, via
    /// <c>libs/fauna-ffi</c>). Each of the two sub-pages builds its OWN machine
    /// instance (the windows per-page-machine convention — mirrors
    /// Devices/Folders, not linux's per-settings-shell shared-machine choice);
    /// <paramref name="observer"/> ticks on every snapshot change; each page
    /// re-renders off <c>Snapshot()</c>. content-moderation-and-ranking.md §
    /// Tier-3 community models.
    /// </summary>
    Task<LabelerCatalogMachine> BuildLabelerCatalogMachineAsync(LabelerCatalogObserver observer);

    /// <summary>
    /// Build the shared-Rust <c>MediaMachine</c> (the Media content-plane explorer)
    /// bound to this session's connected WS-RPC requester — the same
    /// <see cref="BuildDevicesMachineAsync"/> reuse-the-shared-connection pattern.
    /// The machine owns the cross-set <c>fauna.media.list</c> read + the client-held
    /// <c>media-sort-select</c> / <c>media-folder-filter</c> / <c>media-view-toggle</c>
    /// view state, deriving a <c>MediaPageSnapshot</c> (media.md § State &amp; data
    /// shape; rule 2 — observer-driven rendering off the shared snapshot). All
    /// sort/filter logic runs in shared Rust (<c>libs/fauna-media-machine</c>); the
    /// page is pure glue. <paramref name="observer"/> ticks on every snapshot change;
    /// the page re-renders off <c>Snapshot()</c>.
    /// </summary>
    Task<MediaMachine> BuildMediaMachineAsync(MediaObserver observer);

    /// <summary>
    /// Inject the followed-public-folder source into a built
    /// <see cref="MediaMachine"/> — the Media half of the seam
    /// <see cref="WireDevicesForeignSetsAsync"/> is the Devices half of
    /// (media.md § Followed public folders). Best-effort, beside
    /// <see cref="BuildMediaMachineAsync"/>.
    /// <para>
    /// ⚠ Skip it and <c>MediaPageSnapshot.followed</c> is PERMANENTLY empty:
    /// the filter offers no followed scope and both followed gestures have
    /// nothing to act on — a compiles-and-is-unreachable failure the wasm face
    /// shipped for a day before this was written down. It is not carried by a
    /// binding regen either: <c>set_followed_media_source</c> takes an
    /// <c>Arc&lt;dyn FollowedMediaSource&gt;</c>, which UniFFI cannot express,
    /// so the hand-written <c>wire_media_followed_folders</c> is the only door.
    /// </para>
    /// </summary>
    Task WireMediaFollowedFoldersAsync(MediaMachine media);

    /// <summary>
    /// Build the shared-Rust <c>MailPolicyMachine</c> (the admin-mail page's
    /// flat policy form) bound to this session's connected WS-RPC requester —
    /// the same <c>BuildDevicesMachineAsync</c> reuse-the-shared-connection
    /// pattern. The admin-mail <c>put_*</c> / <c>get_mail_config</c> kinds are
    /// Admin-class; the shared connection authenticates as the logged-in actor,
    /// which on this (am_i_admin-gated) page is the admin. Riding the shared,
    /// auto-reconnecting client avoids the prior one-shot per-page
    /// <c>FfiNestClient.Connect()</c> that surfaced a transient os-error-10061 as
    /// a page error while other pages recovered.
    /// </summary>
    Task<MailPolicyMachine> BuildMailPolicyMachineAsync();

    /// <summary>
    /// Build the shared-Rust <c>CaldavPolicyMachine</c> (the admin-calendar page's
    /// deployment-wide CalDAV-enable toggle, admin.md § 8 Calendar) bound to this
    /// session's connected, auto-reconnecting WS-RPC requester — the CalDAV-enable
    /// sibling of <see cref="BuildMailPolicyMachineAsync"/>. The
    /// <c>get_mail_config</c> read twin (hydrates <c>caldav_enabled</c>) +
    /// <c>set_caldav_enabled</c> write are Admin-class; the shared connection
    /// authenticates as the logged-in actor, which on this (am_i_admin-gated) page is
    /// the admin. Same reuse-the-shared-connection pattern.
    /// </summary>
    Task<CaldavPolicyMachine> BuildCaldavPolicyMachineAsync();

    /// <summary>
    /// Build the shared-Rust <c>CarddavPolicyMachine</c> (the admin-contacts
    /// page's deployment-wide CardDAV-enable toggle, admin.md § Contacts) bound
    /// to this session's connected, auto-reconnecting WS-RPC requester — the
    /// CardDAV-enable sibling of <see cref="BuildCaldavPolicyMachineAsync"/>. The
    /// <c>get_mail_config</c> read twin (hydrates <c>carddav_enabled</c>) +
    /// <c>set_carddav_enabled</c> write are Admin-class; the shared connection
    /// authenticates as the logged-in actor, which on this (am_i_admin-gated) page
    /// is the admin. Same reuse-the-shared-connection pattern.
    /// </summary>
    Task<CarddavPolicyMachine> BuildCarddavPolicyMachineAsync();

    /// <summary>
    /// Build the shared-Rust <c>WebdavPolicyMachine</c> (the admin-files page's
    /// deployment-wide WebDAV-enable toggle, admin.md § Files) bound to this
    /// session's connected, auto-reconnecting WS-RPC requester — the WebDAV-enable
    /// sibling of <see cref="BuildCarddavPolicyMachineAsync"/>. The
    /// <c>get_mail_config</c> read twin (hydrates <c>webdav_enabled</c>) +
    /// <c>set_webdav_enabled</c> write are Admin-class; the shared connection
    /// authenticates as the logged-in actor, which on this (am_i_admin-gated) page
    /// is the admin. Same reuse-the-shared-connection pattern.
    /// </summary>
    Task<WebdavPolicyMachine> BuildWebdavPolicyMachineAsync();

    /// <summary>
    /// Build the shared-Rust <c>ForwarderMachine</c> (the admin-aliases page's
    /// external-mail-forwarder list) bound to this session's connected,
    /// auto-reconnecting WS-RPC requester — the same reuse-the-shared-connection
    /// pattern as <see cref="BuildMailPolicyMachineAsync"/>, replacing the prior
    /// one-shot per-page <c>FfiNestClient.Connect()</c>.
    /// </summary>
    Task<ForwarderMachine> BuildForwardersMachineAsync();

    /// <summary>
    /// Build the shared-Rust <c>BridgeApprovalMachine</c> (the
    /// admin-bridges-pending page's pending-mail-bridge approval cards) bound to
    /// this session's connected, auto-reconnecting WS-RPC requester — the same
    /// reuse-the-shared-connection pattern as <see cref="BuildMailPolicyMachineAsync"/>.
    /// </summary>
    Task<BridgeApprovalMachine> BuildBridgeApprovalMachineAsync();

    /// <summary>
    /// Build the credential-loading shared-Rust <c>DnsManagementMachine</c> (the
    /// admin-dns page's record matrix + per-domain mode toggle) bound to this
    /// session's connected, auto-reconnecting WS-RPC requester. The credential
    /// variant (not the read/verify-only build): the per-domain SetMode needs the
    /// tip-sealed <c>fauna.state.dns</c> record, read through the account runtime, and the actor
    /// keypair. The secret is supplied INSIDE the seam (from the session crypto),
    /// never passed by the page — the same reuse-the-shared-connection pattern as
    /// <see cref="BuildMailPolicyMachineAsync"/>.
    /// </summary>
    Task<DnsManagementMachine> BuildDnsManagementMachineWithCredentialsAsync();

    /// <summary>
    /// Build the shared-Rust <c>LocalDomainMachine</c> (the admin-dns page's
    /// domain-CRUD half — add/remove/restore local domains) bound to this session's
    /// connected, auto-reconnecting WS-RPC requester. Read-only w.r.t. credentials
    /// (no secret) — the same reuse-the-shared-connection pattern as
    /// <see cref="BuildMailPolicyMachineAsync"/>.
    /// </summary>
    Task<LocalDomainMachine> BuildLocalDomainsMachineAsync();

    /// <summary>
    /// Build the shared-Rust <c>LinkedNestsMachine</c> bound to this session's
    /// connected, auto-reconnecting WS-RPC requester. The admin-dns cert-issue flow
    /// reads its <c>ThisNest()</c> (the connected nest's identity, the cert's
    /// <c>target_nest_id</c>, uniform across all 7 apps) — the same
    /// reuse-the-shared-connection pattern as <see cref="BuildMailPolicyMachineAsync"/>.
    /// </summary>
    Task<LinkedNestsMachine> BuildLinkedNestsMachineAsync();

    /// <summary>
    /// Build the shared-Rust <c>LinkedNestsMachine</c> WITH the mail relay-provisioning
    /// post-link hook (the user-facing "Linked nests" panel) bound to this session's
    /// connected, auto-reconnecting WS-RPC requester. A both-ends LinkBoth then
    /// auto-provisions the just-linked home box's mailbox reusing the fleet MSEK — the
    /// home-with-public-relay one-action flow (deployment-home-with-public-relay.md
    /// § Pairing). The hook needs the actor secret, supplied INSIDE the seam (from the
    /// session crypto), never by the panel. Falls back to the hook-less
    /// <see cref="BuildLinkedNestsMachineAsync"/> build if keypair derivation throws so
    /// list/link/unlink still work (the hook is a no-op when mail isn't enabled) —
    /// mirrors the linux lead + the panel's prior inline try/catch.
    /// </summary>
    Task<LinkedNestsMachine> BuildLinkedNestsMachineWithMailRelayAsync();

    /// <summary>
    /// Build the shared-Rust <c>LinkedNestsMachine</c> WITH **both** the mail
    /// relay-provisioning post-link hook AND the Nests-page trust facet (the
    /// user-facing "Nests" page, <c>docs/goal/ui/nests.md</c>) bound to this
    /// session's connected, auto-reconnecting WS-RPC requester. The trust seams
    /// (sign the grant-event log, seal the grant ledger) need the
    /// actor secret, supplied INSIDE the seam (from the session crypto), never by
    /// the page. Falls back to the plain <see cref="BuildLinkedNestsMachineAsync"/>
    /// build if keypair derivation throws, so list/link/unlink still work (both
    /// the mail-relay hook and the trust facet are then skipped) — mirrors the
    /// linux lead (<c>apps/fauna-linux/src/settings/linked_nests.rs::wire_machine</c>).
    /// </summary>
    Task<LinkedNestsMachine> BuildLinkedNestsMachineWithMailRelayAndTrustAsync();

    /// <summary>
    /// Build the shared-Rust <c>FfiWebClient</c> (the admin-web page's apex-actor
    /// hosting picker, web-content-hosting.md § Admin apex hosting) bound to this
    /// session's connected, auto-reconnecting WS-RPC requester — the same
    /// reuse-the-shared-connection pattern as <see cref="BuildMailPolicyMachineAsync"/>.
    /// </summary>
    Task<FfiWebClient> BuildWebClientAsync();

    /// <summary>
    /// <c>fauna.admin.factory_reset</c> — stage a new claim code, reply with it,
    /// then exit + restart the nest (the box is wiped, local creds kept). Returns
    /// the staged claim code (the human never sees it; <c>App</c>'s reonboard
    /// handler pre-fills it). Rides the shared, auto-reconnecting WS-RPC requester
    /// rather than a one-shot per-page <c>FfiNestClient.Connect()</c>.
    /// </summary>
    /// <param name="newClaimCode">
    /// <b>Pins</b> the code the wiped box will boot with (the nest honors a pinned
    /// code verbatim). Callers MUST pass a code they have already durably persisted
    /// via <c>MintAndPersistPendingFactoryReset</c> — that is what closes gap CR-1
    /// (<c>nest/common.md</c> § Client-state recoverability): the code used to exist
    /// only in this reply, so a client killed before rendering it left the box
    /// fresh/unclaimed but un-claimable by anyone. Passing <c>null</c> (nest mints a
    /// random code) reopens the gap and is not a supported client path.
    /// </param>
    Task<string> AdminFactoryResetAsync(string? newClaimCode);

    // ── Settings → Mail sub-page machines (shared fauna-client-mail-settings) ──
    // Each panel's machine bound to this session's connected, auto-reconnecting WS-RPC
    // requester — the same reuse-the-shared-connection pattern as the admin builders,
    // replacing the prior per-panel one-shot FfiNestClient.Connect() (which surfaced a
    // transient os-error-10061 as a panel error while shared-client pages recovered).
    // All but MailSettings are User-class (the nest derives the owning actor from the
    // authenticated caller); MailSettings also needs the actor secret + node url (sign
    // submission tokens + key the account plane + derive the MUA details), supplied INSIDE the
    // seam from the session crypto / nest url, never by the panel.

    /// <summary>Build the <c>MailSettingsMachine</c> (Settings → Mail credentials/enable
    /// panel). The secret + node url are supplied inside the seam.</summary>
    Task<MailSettingsMachine> BuildMailSettingsMachineAsync();

    /// <summary>Build the <c>MailSpamMachine</c> (Settings → Mail spam panel).</summary>
    Task<MailSpamMachine> BuildMailSpamMachineAsync();

    /// <summary>Build the <c>MailListsMachine</c> (Settings → Mail lists panel).</summary>
    Task<MailListsMachine> BuildMailListsMachineAsync();

    /// <summary>Build the <c>MailListMembersMachine</c> scoped to list
    /// <paramref name="listIdHex"/> with display title <paramref name="listName"/>
    /// (Settings → Mail list-members panel).</summary>
    Task<MailListMembersMachine> BuildMailListMembersMachineAsync(string listIdHex, string listName);

    /// <summary>Build the <c>MailExportMachine</c> (Settings → Mail export panel) with key
    /// custody: <paramref name="handle"/> names the archive (the VM refreshes it before
    /// every Start / Resume / Download), and <paramref name="saveDir"/> is where
    /// § Download flow step 5 writes the recovered <c>.zip.zst</c>.</summary>
    Task<MailExportMachine> BuildMailExportMachineAsync(string handle, string saveDir);

    /// <summary>Build the <c>MailImportMachine</c> (Settings → Mail import wizard).</summary>
    Task<MailImportMachine> BuildMailImportMachineAsync();

    /// <summary>Build the <c>MailAliasesMachine</c> (Settings → Mail aliases panel).</summary>
    Task<MailAliasesMachine> BuildMailAliasesMachineAsync();

    // ── Settings → AT Protocol (shared fauna-atproto-settings-machine) ───────────

    /// <summary>
    /// Build the shared-Rust <c>AtprotoSettingsMachine</c> — the ONE machine for the
    /// whole AT Protocol page (<c>ui/atproto.md</c> § State &amp; data shape: "Two machines
    /// driving one page's levels would duplicate the level state and collide; do not
    /// build a second"). It carries the integration-depth selector, the transition
    /// card, the hosted panel and the F1 login-plane rows in one snapshot.
    ///
    /// <para>Takes the actor secret like <see cref="BuildMailSettingsMachineAsync"/>:
    /// the app credentials it mints are custodied in the actor's sealed
    /// <c>fauna.state.atproto</c> store, so the nest can never recover them (libs/fauna-ffi/src/
    /// atproto_settings.rs). <paramref name="observer"/> ticks on every snapshot
    /// change.</para>
    /// </summary>
    Task<IAtprotoSettingsMachine> BuildAtprotoSettingsMachineAsync(AtprotoSettingsObserver observer);

    // ── Settings → Connected apps (shared fauna-client-connected-apps) ───────────

    /// <summary>
    /// Build the shared-Rust <c>ConnectedAppsMachine</c> for the Connected apps page
    /// (<c>docs/goal/ui/connected-apps.md</c>) over this session's nest connection.
    /// <paramref name="mail"/> is the session's own Mail &amp; Calendar machine (already
    /// hydrated): the mail app passwords are rows of the roster, read, revoked and revealed
    /// through it (<c>null</c> builds a roster without them). Every call the machine makes is a
    /// plain authenticated nest request, so the build needs no secret — except an approve naming
    /// a <c>fauna:records:</c> scope, which mints the app's consent-time grant under the actor's
    /// own seed; that wiring is applied here and is best-effort (a build without the account
    /// runtime refuses it, and such an approve is then refused rather than resolved keyless).
    /// A fresh machine per call: the page starts every visit unread.
    /// </summary>
    Task<IConnectedAppsMachine> BuildConnectedAppsMachineAsync(
        ConnectedAppsObserver observer, MailSettingsMachine? mail);

    // ── CardDAV Address Book (contacts.md § Address Book segment; slice 4b) ──
    // Read-only native view over the MDA-sealed vCard store — a SEPARATE store
    // from the social contact graph above (carddav-server.md § Independent
    // enablement). Thin wrappers over the shared FfiCarddavClient, mirroring the
    // FoldersMembersListActorsAsync shape: construct FfiCarddavClient fresh per
    // call (cheap — no session/secret args at the call site).

    /// <summary>
    /// <c>fauna.bridges.list_addressbooks</c> + unseal each row's metadata via
    /// <c>FfiCarddavClient.ListAddressbooks</c>. Empty when mail/CardDAV is off, or
    /// the actor has no book yet (no lazy provisioning on this read seam).
    /// </summary>
    Task<FfiAddressbookRow[]> CarddavListAddressbooksAsync();

    /// <summary>
    /// Query + decode every vCard in one address book via
    /// <c>FfiCarddavClient.QueryCards</c>. Empty when mail/CardDAV is off, or the
    /// book has no row yet (<c>AddressbookNotFound</c> → empty).
    /// </summary>
    Task<FfiCardRow[]> CarddavQueryCardsAsync(string addressbookIdHex);

    /// <summary>
    /// Resolve a <c>SearchNav.Contact</c> hit's <c>uid_hash</c> to its holding
    /// book + server-assigned <c>card_id</c> via
    /// <c>FfiCarddavClient.LocateCardByUidHash</c> (search.md § Where logic
    /// lives → Result navigation). The index's identity (<c>uid_hash</c>,
    /// stable across an in-place vCard edit) and the Address Book's own key
    /// (<c>card_id</c>) are different id spaces of the same width — never a
    /// client-side scan or a re-derived join (the id-space trap this row's
    /// diff repeats verbatim). <c>found</c> is <c>null</c> when no book holds
    /// the identity (deleted, or a stale index row).
    /// </summary>
    Task<FfiLocatedCard> CarddavLocateCardByUidHashAsync(string uidHashHex);

    // ── Task delegation (participants.md § Task delegation) ────────────────
    // The Settings → Task delegation sub-page: per heavy-task-kind runner +
    // assignment picker, over the shared fauna_client_delegation::TaskDelegationView.
    // The secret comes from session crypto (never the page); deviceId (hex) is
    // supplied by the caller — the same ISessionAccount.DeviceId plumbing as
    // BackupDestinationStatusAsync above.

    /// <summary>
    /// <c>FfiNestClient.TaskDelegationViewForDevice(...).Load()</c> — the composed per-kind
    /// rows (<c>fauna.state.delegation</c> pins + the live <c>fauna.delegation.observe</c>
    /// lease). windows passes <c>FfiHeavyTaskCapability.IndexOnly</c> (2026-08-16, the
    /// slice-5 flip): a native desktop that runs the content-index builder but ships
    /// NO segment-backup upload driver, so "This device" is offered for <c>index</c>
    /// and withheld for <c>backup-upload</c>, which the source nest writes
    /// (backup-restore.md § Background Tasks → Flip status (slice 5)). Declaring
    /// <c>Runner</c> here after the driver's deletion would offer a self-pin nothing
    /// on the box can honour — a user who took it would silently stop being backed up.
    /// </summary>
    Task<FfiTaskDelegationRow[]> TaskDelegationListAsync(string deviceId);

    /// <summary>
    /// <c>FfiNestClient.TaskDelegationViewForDevice(...).SetAssignment(...)</c> — persist a
    /// pin change for <paramref name="taskKind"/> via the <c>fauna.state.delegation</c>
    /// read-modify-write.
    /// </summary>
    Task TaskDelegationSetAssignmentAsync(string deviceId, string taskKind, FfiPinOption option);

    // ── Recovery kit + the stolen-identity succession ──────────────────────
    // The Settings → Account "Recovery Kit" section (settings.md § Recovery kit),
    // over `libs/fauna-ffi/src/recovery.rs` — which itself composes the shared
    // `fauna_client_recovery` ceremonies every other app runs. Thin passes only,
    // the apple `APIClient.swift` shape: this seam decides NOTHING. The status
    // line's state, which of the actions it enables, and what a succession did
    // all arrive already decided from shared Rust.
    //
    // The identity secret comes from session crypto (never the page), and the
    // account registry from the shell's one credential store — the same store
    // the switcher reads, so a seed the ceremony persists is the seed the next
    // launch activates.

    /// <summary>
    /// <c>recovery_kit_status</c> — the section's state plus every action's
    /// enablement, in one round trip, read from the <b>registration chain</b>
    /// rather than a local flag (so a kit created on another device shows here).
    ///
    /// <para>⚠ The booleans are the point. Never re-derive enablement from
    /// <c>Kind</c>: <c>AllowsStolen</c> is unconditionally true (theft is exactly
    /// the no-kit case) and <c>AllowsReplace</c> stays true <i>during</i> a
    /// pending window.</para>
    /// </summary>
    Task<FfiRecoveryKitStatus> RecoveryKitStatusAsync();

    /// <summary>
    /// <c>recovery_create_kit</c> — <c>recovery-kit-create-button</c> and
    /// <c>recovery-kit-replace-button</c> in ONE call, because they are one
    /// ceremony with two authorization arms: <paramref name="heldKitInput"/> null
    /// is the first registration, non-null the replace authorized by the prior
    /// key. Which arm is legal follows from the status the section already read.
    /// </summary>
    Task<FfiMintedKit> RecoveryCreateKitAsync(string? heldKitInput);

    /// <summary>
    /// <c>recovery_request_seed_alone_replacement</c> —
    /// <c>recovery-kit-lost-button</c>. Opens the 30-day window rather than
    /// taking effect now, but still mints a secret that must be shown at once.
    /// </summary>
    Task<FfiMintedKit> RecoveryRequestSeedAloneReplacementAsync();

    /// <summary>
    /// <c>recovery_veto_pending_replacement</c> —
    /// <c>recovery-pending-veto-button</c>. Returns whether something was
    /// actually pending to cancel.
    /// </summary>
    Task<bool> RecoveryVetoPendingReplacementAsync(string heldKitInput);

    /// <summary>
    /// <c>recovery_reseal_escrow_with_held_kit</c> —
    /// <c>recovery-kit-escrow-reseal-button</c>, the no-escrow repair. Restores
    /// phrase recovery <b>without</b> retiring the kit in hand, which is why it
    /// is deliberately not a create. Returns the unix seconds the blob landed.
    /// </summary>
    Task<long> RecoveryResealEscrowWithHeldKitAsync(string heldKitInput);

    /// <summary>
    /// <c>succession_succeed_with_held_kit</c> — <c>identity-stolen-button</c>'s
    /// ONE call. Re-points the account to a freshly minted successor, persists
    /// and read-back-verifies its seed, sweeps the old identity's MLS groups and
    /// records the predecessor → successor link — all inside the FFI boundary,
    /// because the ceremony's correctness is almost entirely its order.
    ///
    /// <para><b>Irreversible</b>, and the caller must have gated it behind
    /// <c>identity-stolen-confirm-field</c> reading the literal <c>SUCCEED</c>.
    /// An <c>Err</c> means the succession did not land; the failure arms that DO
    /// carry a seed come back as a result whose <c>Persisted</c> tells the truth
    /// about it — and on <c>Persisted == false</c> the caller must put the secret
    /// on screen and <b>not</b> tear the session down.</para>
    /// </summary>
    Task<FfiLandedSuccession> SuccessionSucceedWithHeldKitAsync(string kitInput);

    /// <summary>
    /// <c>run_succession_aftermath</c> — the post-succession aftermath: legs 1, 2,
    /// 4, 7, 6, in the order and with the barriers the shared driver owns
    /// (<c>succession-aftermath.md</c> § Re-key scope's <c>BackupKey</c> corpus
    /// row). The app supplies only what it alone holds — this session's secret,
    /// its nest URL, its data dir and the account registry; the ceremony's raise
    /// context is parked in that registry by shared Rust.
    ///
    /// <para>The <b>sibling half</b> of
    /// <see cref="SuccessionSucceedWithHeldKitAsync"/> above: that hands the
    /// account to the successor and deliberately stops before the aftermath; this
    /// is the pass the successor's first session then owes its inherited corpus.
    /// Best-effort by contract — see <c>Helpers.SuccessionAftermath.RunAsync</c>,
    /// the one caller, and call it from the universal post-auth hook on
    /// <b>every</b> authenticated start rather than only after a ceremony.</para>
    /// </summary>
    Task<FfiAftermathOutcome> RunSuccessionAftermathAsync();

    /// <summary>
    /// <c>succession_retry_group_sweep</c> — <c>recovery-kit-sweep-retry-button</c>:
    /// finish a group sweep the ceremony left unfinished (<c>settings.md</c>
    /// § Recovery kit → <i>Finishing an unfinished group sweep</i>).
    ///
    /// <para>Never throws for an ordinary refusal — every arm, including a
    /// transport failure, comes back as an <see cref="FfiSweepRetryAnswer"/>
    /// carrying its own sentence; what CAN throw is reaching the nest at all. The
    /// old-identity resolver is deliberately the PURE
    /// <c>RetiredIdentityStorePath</c>, never <c>SuccessorStorePath</c> — see that
    /// type's doc for why the retired identity's resolver must create
    /// nothing.</para>
    /// </summary>
    Task<FfiSweepRetryAnswer> SuccessionRetryGroupSweepAsync();

    /// <summary>
    /// <c>succession_discharge_owed_sweep</c> — the unbidden press a <b>relaunch
    /// adoption</b> owes (<c>succession-propagation.md</c> § Propagation → <i>Own
    /// device fleet</i>, the relaunch-adoption clause): the same ceremony as
    /// <see cref="SuccessionRetryGroupSweepAsync"/>, answering with the report to
    /// park as well — shared Rust picks it, never an empty one.
    /// </summary>
    Task<FfiOwedSweepAnswer> SuccessionDischargeOwedSweepAsync();

    // ── T16 custody facet (devices.md § Custody facet, pieces 1–3 + the mint) ──
    // One seam method per UniFFI export (libs/fauna-ffi/src/custody.rs,
    // devices.rs); every act runs the shared run_custody_act and answers with
    // the re-folded facet + an error (DevicesCustodyFacet paints both).

    /// <summary><c>custody_facet_load</c> — the owner-side "who holds my data"
    /// fold (`custody-holder-*`), over this session's own secret (mirrors
    /// <see cref="RunSuccessionAftermathAsync"/>'s shape — no secret param).
    /// <c>null</c> = the config was unreadable this pass (transient) — the
    /// caller must keep its previously-painted rows rather than blanking a
    /// live list.</summary>
    Task<CustodyFacetView?> CustodyFacetLoadAsync();

    /// <summary><c>custody_revoke</c> — the `custody-holder-revoke-button`
    /// gesture. Takes the row's grant id and accept-bound custodian key
    /// (never a row index, which a refold can re-point). The outcome's
    /// <c>Error</c> is a soft failure inside a successful call (never a
    /// silent drop — e2e convention 11); <c>Facet</c> is the re-folded state,
    /// applied whether or not <c>Error</c> is set.</summary>
    Task<FfiCustodyActOutcome> CustodyRevokeAsync(byte[] grantId, byte[]? holder);

    /// <summary><c>custody_drive</c> — one ceremony drive pass, fire-and-forget
    /// (deposits nothing to await; a receipt landed since the last visit is
    /// already in the next <see cref="CustodyFacetLoadAsync"/> read). Call on
    /// the page's own load edge, before the fold — what makes owner-side
    /// receipt freshness real.</summary>
    Task CustodyDriveAsync(ConversationsSession session);

    /// <summary><c>custody_accept</c> — <c>custody-offer-accept-button</c>.
    /// <paramref name="onNest"/> is the target select's answer (binds the host's
    /// pinned NEST identity; false binds this device's principal). The session
    /// is required: the accept is posted by the drive pass over it.</summary>
    Task<FfiCustodyActOutcome> CustodyAcceptAsync(ConversationsSession session, byte[] grantId, bool onNest);

    /// <summary><c>custody_decline</c> — <c>custody-offer-decline-button</c>.</summary>
    Task<FfiCustodyActOutcome> CustodyDeclineAsync(byte[] grantId);

    /// <summary><c>custody_set_budget</c> — the <c>custody-held-budget-input</c>
    /// commit; <paramref name="cap"/> is already parsed by the shared
    /// <c>parse_byte_size</c>.</summary>
    Task<FfiCustodyActOutcome> CustodySetBudgetAsync(byte[] grantId, ulong cap);

    /// <summary><c>custody_stop</c> — <c>custody-held-stop-button</c> (pauses
    /// the hold, keeps the bytes).</summary>
    Task<FfiCustodyActOutcome> CustodyStopAsync(byte[] grantId);

    /// <summary><c>custody_remove</c> — <c>custody-held-remove-button</c> (the
    /// reclaim).</summary>
    Task<FfiCustodyActOutcome> CustodyRemoveAsync(byte[] grantId);

    /// <summary><c>custody_offer_shows_target_select</c> — whether the consent
    /// card renders <c>custody-offer-target-select</c> (an advertising offer AND
    /// a pinned home-nest identity; absent otherwise, never disabled). A local
    /// read of the pin store.</summary>
    Task<bool> CustodyOfferShowsTargetSelectAsync(CustodyOfferRowView offer);

    /// <summary><c>custody_mint_candidates</c> — the mint flow's host options,
    /// one per 1:1 conversation the request could travel over. Empty = no one
    /// to ask yet (the caller says so rather than opening an empty picker).</summary>
    IReadOnlyList<CustodyMintCandidateView> CustodyMintCandidates(ConversationsSession session);

    /// <summary><c>custody_mint</c> — <c>custody-mint-confirm-button</c> over a
    /// <see cref="CustodyMintCandidates"/> row's <c>host</c> + <c>channel_hex</c>,
    /// passed back unchanged.</summary>
    Task<FfiCustodyActOutcome> CustodyMintAsync(ConversationsSession session, byte[] host, string channelHex);

    /// <summary><c>devices_keyless_posture</c> — piece 1's marker for each
    /// roster principal, in order (<c>true</c> = paint
    /// <c>device-keyless-posture-badge</c>). A local read of the account store;
    /// every fail-safe answers <c>false</c>.</summary>
    Task<IReadOnlyList<bool>> DevicesKeylessPostureAsync(IReadOnlyList<string?> principals);

    // ── Devices page: the standing enrollment notice (ui/devices.md § State &
    // data shape + § Errors & edge cases) ──

    /// <summary><c>account_enrollment_notice</c> — the sentence to paint on
    /// Settings → Devices <c>error-message</c> while the nest refuses to enroll
    /// this machine (today: the tier device cap), already localized by shared
    /// Rust, or <c>null</c> when nothing stands (also before the account runtime
    /// is assembled — nothing to say yet, not an error). A local slot read, never
    /// a network call. A <c>null</c> is an ANSWER — the caller must clear a
    /// notice it painted; only an exception means "unknown, keep what you
    /// painted" (see <c>DevicesEnrollmentNotice</c>).</summary>
    Task<string?> AccountEnrollmentNoticeAsync();

    /// <summary><c>devices_this_device_row</c> — the roster row the Devices
    /// page's <c>device-this-mark-badge</c> marks (behavior/devices.md
    /// § This-device marker): the row this machine's enrollment latched on,
    /// else <paramref name="ownDeviceId"/> (the app's own
    /// <c>ISessionAccount.DeviceId</c>), else <c>null</c>. The rule —
    /// <i>enrolled wins; the own id is the fallback</i> — is shared Rust
    /// (<c>fauna_devices_machine::this_device_row</c>); nothing here decides it.
    /// A local slot read, never a network call.</summary>
    Task<string?> ThisDeviceRowAsync(string? ownDeviceId);
}
