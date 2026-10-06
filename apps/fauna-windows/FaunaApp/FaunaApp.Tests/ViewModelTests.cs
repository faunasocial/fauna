using System.Text.Json;
using Xunit;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

// ── MainViewModel Tests ──

public class MainViewModelTests
{
    [Fact]
    public async Task Load_SetsNeedsOnboarding_WhenNoIdentity()
    {
        var rpc = new MockNestRpcClient { NextIdentity = null };
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.NeedsOnboarding);
    }

    [Fact]
    public async Task Load_PopulatesActorId_WhenIdentityExists()
    {
        var rpc = new MockNestRpcClient
        {
            NextIdentity = new IdentityInfo("deadbeef", "alice", "https://example.com")
        };
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.NeedsOnboarding);
        Assert.Equal("deadbeef", vm.ActorId);
        Assert.Equal("alice", vm.Handle);
    }

    [Fact]
    public void NavigateTo_UpdatesCurrentPage()
    {
        var vm = new MainViewModel(new MockNestRpcClient(), new FakeAgentStatusProbe());

        vm.NavigateToCommand.Execute("Contacts");

        Assert.Equal("Contacts", vm.CurrentPage);
    }

    [Fact]
    public void DefaultPage_IsStatus()
    {
        var vm = new MainViewModel(new MockNestRpcClient(), new FakeAgentStatusProbe());

        Assert.Equal("Status", vm.CurrentPage);
    }

    [Fact]
    public async Task Load_CallsGetIdentity()
    {
        var rpc = new MockNestRpcClient { NextIdentity = null };
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Contains("GetIdentity", rpc.Calls);
    }
}

// ── OnboardingViewModel Tests ──
//
// Removed in the handle-first onboarding rewrite. The old per-step tests
// targeted `int Step`, `IRegistryClient`, and the in-VM provisioning flow,
// none of which exist anymore — the wizard is now a thin proxy over the
// shared UniFFI OnboardingMachine. New tests for the shared state machine
// live in the Rust crate (libs/fauna-onboarding-machine).

// Conversations VMs are now thin observers over the shared ConversationsManager
// (see libs/fauna-conversations) — unit tests for the conversations domain live
// in the Rust crate (`cargo test -p fauna-conversations`). Removed:
// - ConversationsViewModelTests (NestHttpClient-driven load path is gone)
// - ConversationDetailViewModelTests (replaced by ConversationsViewModel.SelectedDetail)

// ── ContactsViewModel Tests ──

public class ContactsViewModelTests
{
    // Find-user classification is single-sourced in shared Rust
    // (fauna_core::resolve::classify_recipient, UniFFI classifyRecipient) so no
    // client re-derives the 64-hex / user@domain rule (priority #2/#3/#4). Calls the
    // real FFI (native fauna_ffi dll loads in the test host); a client-side rejection
    // must never reach the roster lookup. The handle branch (resolve_nest →
    // resolve_handle) is a live anonymous network resolve, exercised via
    // ClassifyRecipientInput (below) + e2e, not a unit-mockable RPC.
    [Fact]
    public async Task LookUpRecipient_RejectsInvalidInput()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ContactsViewModel(rpc);

        // 64 chars but not hex and no `@` — classify_recipient classifies "invalid".
        await vm.LookUpRecipientCommand.ExecuteAsync(new string('z', 64));

        Assert.NotNull(vm.ActorIdError);
        Assert.Null(vm.ActorIdResult);
        Assert.DoesNotContain("ContactsList", rpc.Calls);
    }

    [Fact]
    public async Task LookUpRecipient_AcceptsValid64HexActorId()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ContactsViewModel(rpc);

        await vm.LookUpRecipientCommand.ExecuteAsync(new string('a', 64));

        Assert.Null(vm.ActorIdError);
        Assert.NotNull(vm.ActorIdResult); // Pending stub from the (empty) roster
        Assert.Contains("ContactsList", rpc.Calls);
    }

    [Fact]
    public async Task Load_PopulatesContactsAndKnocks()
    {
        // Roster + knocks ride WS-RPC (fauna.{contacts,knocks}.list).
        var rpc = new MockNestRpcClient
        {
            NextContacts = new List<ContactInfo>
            {
                new("actor1", null, ContactStatus.Accepted, 1000),
            },
            NextKnocks = new List<KnockInfo>
            {
                new("actor2", "Hi there", 2000),
            }
        };
        var vm = new ContactsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Single(vm.Contacts);
        Assert.Equal("actor1", vm.Contacts[0].ActorId);
        Assert.Single(vm.Knocks);
        Assert.Equal("actor2", vm.Knocks[0].ActorId);
        Assert.Equal("Hi there", vm.Knocks[0].Summary);
        Assert.Contains("ContactsList", rpc.Calls);
        Assert.Contains("KnocksList", rpc.Calls);
        Assert.False(vm.IsLoading);
    }

    // succession-aftermath.md § Propagation → *Removing a flagged member*: the
    // owner's open review roster, cached alongside Contacts/Knocks so
    // contact-unattested-mark can answer per-row without a hand-rolled scan.
    [Fact]
    public async Task Load_PopulatesMemberReviewRoster()
    {
        var rpc = new MockNestRpcClient
        {
            NextMemberReviews = new List<FfiMemberReview>
            {
                new(new byte[32], new[] { "compromise_window" }),
            },
        };
        var vm = new ContactsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Single(vm.MemberReviewRoster);
        Assert.Contains("MemberReviewsList", rpc.Calls);
    }

    [Fact]
    public async Task AcceptKnock_CallsRpc_WithPeerId()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ContactsViewModel(rpc);

        await vm.AcceptKnockCommand.ExecuteAsync("actor1");

        Assert.Contains("KnocksAccept", rpc.Calls);
        Assert.Equal("actor1", rpc.LastKnockPeer);
    }

    [Fact]
    public async Task BlockKnock_CallsRpc_WithPeerId()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ContactsViewModel(rpc);

        await vm.BlockKnockCommand.ExecuteAsync("actor1");

        Assert.Contains("KnocksBlock", rpc.Calls);
        Assert.Equal("actor1", rpc.LastKnockPeer);
    }

    [Fact]
    public async Task ConfirmContact_CallsRpc_WithPeerId_AndReloadsRoster()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ContactsViewModel(rpc);

        await vm.ConfirmContactCommand.ExecuteAsync("actor1");

        Assert.Contains("ContactsConfirm", rpc.Calls);
        Assert.Equal("actor1", rpc.LastContactsConfirmPeer);
        // The roster reload contacts.md § User actions requires ("status label
        // flips to confirmed on refetch") — ConfirmContactAsync must re-pull,
        // not just fire-and-forget the confirm call.
        Assert.Contains("ContactsList", rpc.Calls);
    }

    [Fact]
    public async Task DismissKnock_CallsRpc_WithPeerId()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ContactsViewModel(rpc);

        await vm.DismissKnockCommand.ExecuteAsync("actor1");

        Assert.Contains("KnocksDismiss", rpc.Calls);
        Assert.Equal("actor1", rpc.LastKnockPeer);
    }

    [Fact]
    public async Task LookUpRecipient_ActorId_ResolvesFromRoster()
    {
        var rpc = new MockNestRpcClient
        {
            NextContacts = new List<ContactInfo> { new("ab".PadRight(64, 'a'), null, ContactStatus.Accepted, 1000) },
        };
        var vm = new ContactsViewModel(rpc);

        await vm.LookUpRecipientCommand.ExecuteAsync("ab".PadRight(64, 'a'));

        Assert.Contains("ContactsList", rpc.Calls);
        Assert.NotNull(vm.ActorIdResult);
        Assert.Equal(ContactStatus.Accepted, vm.ActorIdResult!.Status);
    }

    [Fact]
    public async Task LookUpRecipient_ActorId_FallsBackToPending_WhenNotOnRoster()
    {
        var rpc = new MockNestRpcClient(); // empty roster
        var vm = new ContactsViewModel(rpc);

        await vm.LookUpRecipientCommand.ExecuteAsync("cd".PadRight(64, 'c'));

        Assert.NotNull(vm.ActorIdResult);
        Assert.Equal(ContactStatus.Pending, vm.ActorIdResult!.Status);
    }

    // contacts-search (SearchContactsAsync / SearchResults) was REMOVED: the
    // contacts-search-field is a page-side local roster filter (matching linux),
    // not a nest search, and full-text hits carry opaque FTS keys (not actor ids)
    // so they can't drive add-contact. The standalone Search page covers
    // fauna.search.query — see SearchViewModelTests below.

    [Fact]
    public async Task AddFoundContact_SendsKnock_OverRpc()
    {
        // Add-contact composes a signed (ContactRequest, Post) knock and sends it
        // over fauna.inbox.send (NestRpcClient.SendKnockAsync) — the WS-RPC successor
        // of the retired POST /api/v1/inbox/{actor} twin (federation.md § Federation
        // residue surface).
        var rpc = new MockNestRpcClient();
        var vm = new ContactsViewModel(rpc);

        await vm.AddFoundContactCommand.ExecuteAsync("actor9");

        Assert.Contains("SendKnock", rpc.Calls);
        Assert.Equal("actor9", rpc.LastKnockSent);
    }
}

// ── NotificationsViewModel Tests ──

public class NotificationsViewModelTests
{
    [Fact]
    public async Task Load_PopulatesNotificationsAndCount_OverRpc()
    {
        var rpc = new MockNestRpcClient
        {
            NextNotifications = FfiNotifListReplyFixture.Make(
                notifications: new[]
                {
                    MockNestRpcClient.MakeNotif(1, "like", "alice liked your post", false, 1000),
                    MockNestRpcClient.MakeNotif(2, "reply", "bob replied", true, 2000),
                },
                cursor: 2),
            NextUnreadCount = 5,
        };
        var vm = new NotificationsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(2, vm.Notifications.Count);
        Assert.Equal("alice liked your post", vm.Notifications[0].Text);
        Assert.Equal("like", vm.Notifications[0].Type);
        Assert.Equal(5, vm.UnreadCount);
        Assert.Contains("NotificationsList", rpc.Calls);
        Assert.Contains("NotificationsCount", rpc.Calls);
        Assert.False(vm.HasMore); // fewer than a full page of 25
    }

    [Fact]
    public async Task MarkRead_CallsRpc_AndZeroesCount()
    {
        var rpc = new MockNestRpcClient { NextUnreadCount = 7 };
        var vm = new NotificationsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.MarkReadCommand.ExecuteAsync(null);

        Assert.Contains("NotificationsMarkRead", rpc.Calls);
        Assert.Equal(0, vm.UnreadCount);
    }
}

// SyncViewModelTests removed — SyncViewModel was dead code (no production page
// constructed it). The Media page now renders ENTIRELY off the shared MediaMachine
// (libs/fauna-media-machine via UniFFI BuildMediaMachine), not a VM-over-seam, so
// there is no MediaViewModel test here — coverage is the e2e chrome test +
// conformance_media_list.rs (mirrors DevicesPage). Folder list/create lives in
// FoldersPage (the Devices / FolderWizard machines).

// ── SettingsViewModel Tests ──

public class SettingsViewModelTests
{
    [Fact]
    public async Task Load_PopulatesIdentity()
    {
        var rpc = new MockNestRpcClient
        {
            NextIdentity = new IdentityInfo("abc123", "alice", "https://nest.example.com")
        };
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("abc123", vm.ActorId);
        Assert.Equal("alice", vm.Handle);
        Assert.Equal("https://nest.example.com", vm.NestUrl);
        Assert.Equal("https://nest.example.com", vm.NewNestUrl);
    }

    [Fact]
    public async Task Load_PopulatesKeyPackageCount_OverWsRpcSeam()
    {
        // The keypackage POOL count now rides fauna.conversations.keypackage.count
        // (the FfiConversationsClient seam), NOT the dead /api/v1/keypackage HTTP route
        // (conversations.md rule #2 — no client-side MLS).
        var rpc = new MockNestRpcClient
        {
            NextIdentity = new IdentityInfo("abc123", "alice", "https://nest.example.com"),
            NextKeypackageCount = 7,
        };
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(7, vm.KeyPackageCount);
        Assert.True(vm.IsMlsAvailable);
        Assert.Contains("KeypackageCount", rpc.Calls);
    }

    [Fact]
    public async Task Configure_UpdatesNestUrl_OnSuccess()
    {
        var mock = new MockNestHttpClient();
        var vm = new SettingsViewModel(mock, new MockNestRpcClient()) { NewNestUrl = "https://new-nest.example.com" };

        await vm.ConfigureCommand.ExecuteAsync(null);

        Assert.Equal("https://new-nest.example.com", vm.NestUrl);
    }

    [Fact]
    public async Task Configure_SetsError_OnFailure()
    {
        var mock = new MockNestHttpClient { NextError = "invalid URL" };
        var vm = new SettingsViewModel(mock, new MockNestRpcClient()) { NewNestUrl = "not-a-url" };

        await vm.ConfigureCommand.ExecuteAsync(null);

        Assert.Contains("errors/", vm.ErrorMessage);
    }

    [Fact]
    public void InboxMode_DefaultsToUnknownNotAMode()
    {
        // settings.md item 7: unknown is a
        // distinct state from any real mode. A seeded "open" here is a mode the
        // user never chose, indistinguishable to the state protocol from a real
        // fetch that resolved "open" — a relaunch whose fetch is slow (or fails,
        // the caught-and-swallowed InboxModeGetAsync catch) would report a
        // plausible-looking wrong answer instead of an honest unknown.
        var vm = new SettingsViewModel(new MockNestHttpClient(), new MockNestRpcClient());

        Assert.Equal("", vm.InboxMode);
    }

    [Fact]
    public async Task Load_CallsInboxModeGet_OverRpc()
    {
        var rpc = new MockNestRpcClient
        {
            NextIdentity = new IdentityInfo("abc", "alice", "https://example.com"),
            NextInboxMode = "contacts_only",
        };
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Contains("InboxModeGet", rpc.Calls);
        Assert.Equal("contacts_only", vm.InboxMode);
    }

    [Fact]
    public async Task Load_CallsQuotaGet()
    {
        var rpc = new MockNestRpcClient
        {
            NextIdentity = new IdentityInfo("abc", "alice", "https://example.com"),
            NextQuota = MockNestRpcClient.MakeQuota(storageUsed: 1048576, storageMax: 10485760)
        };
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Contains("QuotaGet", rpc.Calls);
        Assert.Equal(1048576, vm.StorageUsedBytes);
        Assert.Equal(10485760, vm.StorageTotalBytes);
        // Via the shared fauna_core::format::quota_percent (value-formatting.md § Quota fraction) —
        // not a hand-rolled `used / total * 100`.
        Assert.Equal(10, vm.StoragePercent);
    }

    [Fact]
    public async Task Load_QuotaGet_ZeroMaxBytes_DoesNotDivideByZero()
    {
        var rpc = new MockNestRpcClient
        {
            NextIdentity = new IdentityInfo("abc", "alice", "https://example.com"),
            NextQuota = MockNestRpcClient.MakeQuota(storageUsed: 0, storageMax: 0)
        };
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(0, vm.StoragePercent);
    }

    [Fact]
    public async Task SetInboxMode_CallsRpc_WithCanonicalMode()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);

        await vm.SetInboxModeByNameCommand.ExecuteAsync("contacts_only");

        Assert.Contains("InboxModeSet", rpc.Calls);
        Assert.Equal("contacts_only", rpc.LastInboxModeSet);
        Assert.Equal("contacts_only", vm.InboxMode);
    }

    [Fact]
    public async Task DeleteAccount_CallsApi()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);

        await vm.DeleteAccountCommand.ExecuteAsync(null);

        Assert.Contains("AccountDelete", rpc.Calls);
    }

    [Fact]
    public async Task ChangeHandle_CallsApi_AndClearsInput()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc) { NewHandle = "bob" };

        await vm.ChangeHandleCommand.ExecuteAsync(null);

        // Settings handle change rides `fauna.profile.handle.change` (the
        // authenticated bearer kind), not the anonymous `register` route.
        Assert.Contains("ChangeHandle", rpc.Calls);
        Assert.Equal(string.Empty, vm.NewHandle);
    }

    [Fact]
    public async Task ChangeHandle_SkipsWhenBlank()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc) { NewHandle = "   " };

        await vm.ChangeHandleCommand.ExecuteAsync(null);

        Assert.DoesNotContain("ChangeHandle", rpc.Calls);
    }

    [Fact]
    public async Task ChangeHandle_InvalidFormat_SetsError_AndSkipsApi()
    {
        // A too-short handle ("ab", < 3 chars) fails the shared canonical
        // validator (fauna_protocol::handle::validate_handle via the UniFFI
        // ValidateHandle face, settings.md § Where logic lives → Handle change).
        // The VM must surface the message in ErrorMessage (routed to the
        // `error-message` element) and NOT round-trip the change RPC — instant,
        // identical feedback across clients (mirrors linux settings/account.rs).
        // A *taken* handle stays server-authoritative (unchanged path).
        var rpc = new MockNestRpcClient();
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc) { NewHandle = "ab" };

        await vm.ChangeHandleCommand.ExecuteAsync(null);

        Assert.DoesNotContain("ChangeHandle", rpc.Calls);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }

    // ── Pending actions (settings.md § Pending actions) — the STANDING
    //    account-page section every other app already carries. Three-state:
    //    null = un-hydrated, empty = hydrated with nothing scheduled, rows =
    //    counted. Never feeds an echoed reply into a local cache; always
    //    re-lists rather than splicing a row out locally.

    [Fact]
    public async Task LoadPendingActions_MapsRowsFromTheSharedRenderer()
    {
        var rpc = new MockNestRpcClient
        {
            NextPendingActions = new List<FfiPendingActionSummary>
            {
                new(7, "handle.change", "newhandle", "pending", 0, 1735689600, 0, Array.Empty<string>()),
            },
        };
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);

        await vm.LoadPendingActionsCommand.ExecuteAsync(null);

        Assert.Contains("PendingActionsList", rpc.Calls);
        var row = Assert.Single(vm.PendingActions!);
        Assert.Equal(7, row.Id);
        // describe_pending_action (shared Rust) renders the verb+target
        // sentence — never re-derived here (priority #2).
        Assert.Contains("newhandle", row.Description);
        Assert.NotEmpty(row.ExecuteAfterText);
    }

    [Fact]
    public async Task LoadPendingActions_EmptyList_IsHydratedNotNull()
    {
        // The three-state distinction: null (un-hydrated) must not be
        // conflated with empty (hydrated, nothing scheduled) — a settled
        // "nothing scheduled" claim before the first list read lands is
        // exactly the dishonesty this section's design forbids.
        var rpc = new MockNestRpcClient();
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);
        Assert.Null(vm.PendingActions);

        await vm.LoadPendingActionsCommand.ExecuteAsync(null);

        Assert.NotNull(vm.PendingActions);
        Assert.Empty(vm.PendingActions!);
    }

    [Fact]
    public async Task CancelPendingAction_CallsApi_AndReloads()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);

        await vm.CancelPendingActionCommand.ExecuteAsync(42L);

        Assert.Contains("PendingActionCancel", rpc.Calls);
        Assert.Contains("PendingActionsList", rpc.Calls);
    }

    [Fact]
    public async Task DeleteAccount_RefreshesPendingActions()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc);

        await vm.DeleteAccountCommand.ExecuteAsync(null);

        Assert.Contains("PendingActionsList", rpc.Calls);
    }

    [Fact]
    public async Task ChangeHandle_RefreshesPendingActions()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SettingsViewModel(new MockNestHttpClient(), rpc) { NewHandle = "bob" };

        await vm.ChangeHandleCommand.ExecuteAsync(null);

        Assert.Contains("PendingActionsList", rpc.Calls);
    }

    // ── Export My Data (settings.md § Data export; account-data-plane.md §
    //    Nest-side requirements item 1, Payload stores decision (5) — the full
    //    archive is the DEFAULT, no toggle). The byte fetch is the HTTP residue
    //    plane (INestHttpClient.ExportAccountDataAsync); the save step goes
    //    through the SAME ISnapshotFileSaver seam single-file restore uses. ──

    private sealed class FakeExportFileSaver : ISnapshotFileSaver
    {
        public readonly List<(string FileName, byte[] Data)> Saved = new();

        public Task<string?> SaveAsync(string suggestedFileName, byte[] data,
            CancellationToken ct = default)
        {
            Saved.Add((suggestedFileName, data));
            return Task.FromResult<string?>($"C:\\fake\\{suggestedFileName}");
        }
    }

    [Fact]
    public async Task ExportAccountData_FetchesArchive_AndSavesAsFaunaExportZip()
    {
        var bytes = new byte[] { 0x50, 0x4b, 3, 4 };
        var http = new MockNestHttpClient { NextExportedData = bytes };
        var saver = new FakeExportFileSaver();
        var vm = new SettingsViewModel(http, new MockNestRpcClient(), fileSaver: saver);

        await vm.ExportAccountDataCommand.ExecuteAsync(null);

        Assert.Contains("ExportAccountData", http.Calls);
        var saved = Assert.Single(saver.Saved);
        Assert.Equal("fauna-export.zip", saved.FileName);
        Assert.Equal(bytes, saved.Data);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task ExportAccountData_SurfacesFetchError_AndSavesNothing()
    {
        var http = new MockNestHttpClient { NextError = "boom" };
        var saver = new FakeExportFileSaver();
        var vm = new SettingsViewModel(http, new MockNestRpcClient(), fileSaver: saver);

        await vm.ExportAccountDataCommand.ExecuteAsync(null);

        Assert.NotNull(vm.ErrorMessage);
        Assert.Empty(saver.Saved);
    }

    [Fact]
    public async Task ExportAccountData_NoFileSaverWired_NoOps()
    {
        // The three non-account settings sub-pages (Privacy/General/Encryption)
        // construct SettingsViewModel with no fileSaver — the command must not
        // fetch bytes it has nowhere to save.
        var http = new MockNestHttpClient { NextExportedData = new byte[] { 1 } };
        var vm = new SettingsViewModel(http, new MockNestRpcClient());

        await vm.ExportAccountDataCommand.ExecuteAsync(null);

        Assert.DoesNotContain("ExportAccountData", http.Calls);
    }
}

// ── BackupsViewModel Tests live near the end of this file (WS-RPC migration). ──

// ── ConflictsViewModel Tests REMOVED 2026-07-11: the ViewModel (the legacy
// keep-local/remote/both chooser, preserved-but-unwired since the 2026-06-28
// unification) is retired with the conflict auto-resolve track — the review
// list renders directly off DevicesMachine.snapshot().conflicts in
// FoldersPage (file-sync.md § Conflicts; ui/folders.md § Conflicts). ──

// ── StatusViewModel Tests ──

public class StatusViewModelTests
{
    [Fact]
    public async Task Load_PopulatesVersionAndConnectionFromNest()
    {
        var nestMock = new MockNestHttpClient
        {
            // Sync sub-object is a nest-side placeholder DirectNestClient can't
            // really answer (it has no visibility into the local per-device sync
            // agent) — give it numbers that must NOT reach the VM, to prove the
            // Sync* properties below come from the local pipe, not this mock.
            NextServiceStatus = new ServiceStatusInfo("1.0.0", 3600, ConnectionState.Connected,
                new SyncStatusInfo(true, true, 999, 999999, 1), null),
        };
        var rpc = new MockNestRpcClient
        {
            NextIdentity = new IdentityInfo("abc123", "alice", "https://nest.example.com"),
        };
        var vm = new StatusViewModel(nestMock, rpc, new FakeAgentStatusProbe());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.NestAvailable);
        Assert.Equal("abc123", vm.ActorId);
        Assert.Equal("alice", vm.Handle);
        Assert.Equal("1.0.0", vm.SyncVersion);
        Assert.Equal(3600UL, vm.SyncUptimeSecs);
        Assert.Equal(ConnectionState.Connected, vm.SyncConnectionState);
        // The local pipe (FakeSyncPipeClient, default ServiceStatus = null = agent
        // unreachable) leaves these at their unknown/default state — NOT the nest
        // mock's 999/999999/true values above.
        Assert.False(vm.SyncConnected);
        Assert.False(vm.Syncing);
        Assert.Equal(0UL, vm.FilesPending);
        Assert.Equal(0UL, vm.BytesPending);
        // Agent unreachable — FakeAgentStatusProbe's default SyncStatus (null) leaves
        // LastSync at its unknown default, never a stale value from a prior read.
        Assert.Null(vm.LastSync);
    }

    [Fact]
    public async Task Load_PopulatesSyncFieldsFromLocalAgent_NotNest()
    {
        // The real per-device Connected/Syncing/FilesPending/BytesPending/LastSync
        // signal comes from the LOCAL sync-agent over the shared FFI surface (mirrors
        // MainViewModel.PollSyncAgentStatusAsync's established pattern) —
        // DirectNestClient talks only to the remote nest and cannot see it.
        var nestMock = new MockNestHttpClient(); // default placeholder Sync, ignored
        var probe = new FakeAgentStatusProbe
        {
            SyncStatus = FfiAgentSyncStatusFixture.Make(
                connected: true, syncing: true, filesPending: 5, bytesPending: 1024, lastSync: 9000),
        };
        var vm = new StatusViewModel(nestMock, new MockNestRpcClient(), probe);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.SyncConnected);
        Assert.True(vm.Syncing);
        Assert.Equal(5UL, vm.FilesPending);
        Assert.Equal(1024UL, vm.BytesPending);
        Assert.Equal(9000UL, vm.LastSync);
    }

    [Fact]
    public void LastSyncText_NullReturnsNeverBaseline()
    {
        // No completed transfer/clean pass yet (a fresh agent, or one that has
        // never synced) reads as the shared "Never" baseline — the same
        // null-baseline idiom as BackupsViewModel.LastUploadText/LastAuditText.
        Assert.Equal(Strings.Get("common/never"), StatusViewModel.LastSyncText(null, 1_700_000_000_000L));
    }

    [Fact]
    public void LastSyncText_SecondsRendersThroughSharedRelativeTime()
    {
        // A real reading is unix SECONDS (the FFI contract) rendered through the
        // shared ValueFormat.RelativeTime, which wants epoch MILLISECONDS —
        // pins the ×1000 conversion, the same trap FileVersionFormatTests guards.
        const long now = 1_700_000_000_000L;
        const ulong secs = 1_699_999_700UL; // 5 minutes before "now"
        var expected = ValueFormat.RelativeTime(now, (long)secs * 1000);
        Assert.Equal(expected, StatusViewModel.LastSyncText(secs, now));
    }

    [Fact]
    public async Task Load_HandlesNestUnavailable()
    {
        var nestMock = new MockNestHttpClient { NextAvailable = false };
        var vm = new StatusViewModel(nestMock, new MockNestRpcClient(), new FakeAgentStatusProbe());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.NestAvailable);
        Assert.Null(vm.ActorId);
    }

    [Fact]
    public async Task Load_WorksWithDefaults()
    {
        var nestMock = new MockNestHttpClient();
        var vm = new StatusViewModel(nestMock, new MockNestRpcClient { NextIdentity = null }, new FakeAgentStatusProbe());

        await vm.LoadCommand.ExecuteAsync(null);

        // Should not throw
        Assert.False(vm.IsLoading);
    }
}

// ── EventsViewModel Tests ──

public class EventsViewModelTests
{
    // Calendars + events ride the encrypted CalDAV store (FfiCaldavClient). Ids
    // are lowercase hex strings end-to-end (no byte round-trip), so "c1" is just
    // the calendar id the create-event path forwards verbatim.
    private static MockNestRpcClient CreateRpcWithCalendars()
        => new MockNestRpcClient
        {
            NextCaldavCalendars = new List<FfiCalendarRow>
            {
                MockNestRpcClient.MakeCaldavCalendar("c1", "Work", "#ff0000"),
            },
        };

    [Fact]
    public async Task Load_PopulatesCalendars()
    {
        var rpc = CreateRpcWithCalendars();
        var vm = new EventsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Single(vm.Calendars);
        Assert.Equal("Work", vm.Calendars[0].Name);
        // Calendars + events ride the encrypted CalDAV store (FfiCaldavClient).
        Assert.Contains("CaldavListCalendars", rpc.Calls);
        // No calendar selected → fans out per owned calendar + the invited feed.
        Assert.Contains("CaldavQueryEvents", rpc.Calls);
        Assert.Contains("CaldavQueryInvitedEvents", rpc.Calls);
    }

    [Fact]
    public async Task Load_PopulatesEvents_FromOwnedCalendarsAndInvited()
    {
        var rpc = CreateRpcWithCalendars();
        rpc.NextCaldavEvents = new List<FfiCalEvent>
        {
            MockNestRpcClient.MakeCaldavEvent(
                "e1", "Meeting", "2026-03-28T10:00:00Z", "2026-03-28T11:00:00Z", calendarIdHex: "c1"),
        };
        rpc.NextCaldavInvited = new List<FfiCalEvent>
        {
            MockNestRpcClient.MakeCaldavEvent("e2", "Invited", "2026-03-29T10:00:00Z", organizedByMe: false),
        };
        var vm = new EventsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(2, vm.Events.Count);
        // Events carry the fake events' hex ids; the calendar color is joined in.
        Assert.Contains(vm.Events, e => e is { Id: "e1", Summary: "Meeting", CalendarColor: "#ff0000" });
        Assert.Contains(vm.Events, e => e is { Id: "e2", Summary: "Invited" });
    }

    [Fact]
    public async Task Load_DedupesEvents_AcrossOwnedAndInvited_ById()
    {
        var rpc = CreateRpcWithCalendars();
        // The same event appears in both the owned-calendar query and the invited
        // feed; the union dedups by hex id (the encrypted store has no cross-cal
        // query, so the VM fans out + dedups).
        rpc.NextCaldavEvents = new List<FfiCalEvent>
        {
            MockNestRpcClient.MakeCaldavEvent("dup", "Shared", "2026-03-28T10:00:00Z"),
        };
        rpc.NextCaldavInvited = new List<FfiCalEvent>
        {
            MockNestRpcClient.MakeCaldavEvent("dup", "Shared", "2026-03-28T10:00:00Z"),
        };
        var vm = new EventsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Single(vm.Events);
    }

    [Fact]
    public async Task Load_SetsErrorMessage_OnException()
    {
        var rpc = new MockNestRpcClient { NextError = "fail" };
        var vm = new EventsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.NotNull(vm.ErrorMessage);
    }

    /// <summary>A pan in MONTH view steps a month. Kept alongside the fixed-date
    /// per-mode theory below because it anchors on <c>DateTimeOffset.Now</c>, so it
    /// also exercises the December→January rollover the theory's fixed March date
    /// never reaches.</summary>
    [Fact]
    public async Task NavigateInMonthView_IncrementsMonth()
    {
        var vm = new EventsViewModel(CreateRpcWithCalendars())
        {
            ViewMode = FfiCalendarViewMode.Month,
        };
        var initialMonth = vm.CurrentMonth.Month;

        await vm.NavigateCommand.ExecuteAsync(1);

        var expected = initialMonth == 12 ? 1 : initialMonth + 1;
        Assert.Equal(expected, vm.CurrentMonth.Month);
    }

    [Fact]
    public void MonthYearDisplay_InMonthView_ReflectsCurrentMonth()
    {
        var vm = new EventsViewModel(new MockNestRpcClient()) { ViewMode = FfiCalendarViewMode.Month };

        Assert.Contains(vm.CurrentMonth.ToString("MMMM"), vm.MonthYearDisplay);
        Assert.Contains(vm.CurrentMonth.Year.ToString(), vm.MonthYearDisplay);
    }

    /// <summary><c>calendar-date-label</c> describes the VISIBLE RANGE, not always
    /// the month (events.md § Where logic lives → *View mode + visible range*).
    ///
    /// <para>This is the half that makes the pan observable at all: while the
    /// label rendered the month in every mode, seven day-view pans inside one
    /// month left it unchanged — and the e2e helper WAITS for it to change, so a
    /// month-only label times the helper out rather than mis-asserting. The
    /// assertion is that the four modes produce FOUR DISTINCT strings from one
    /// fixed date, which is exactly what a month-only label cannot do.</para></summary>
    [Fact]
    public void MonthYearDisplay_DescribesTheVisibleRange_PerViewMode()
    {
        var anchor = new DateTimeOffset(2026, 3, 17, 0, 0, 0, TimeSpan.Zero);  // a Tuesday
        var vm = new EventsViewModel(new MockNestRpcClient())
        {
            CurrentMonth = anchor,
            CurrentDay = anchor,
        };

        vm.ViewMode = FfiCalendarViewMode.Month;
        var month = vm.MonthYearDisplay;
        vm.ViewMode = FfiCalendarViewMode.Week;
        var week = vm.MonthYearDisplay;
        vm.ViewMode = FfiCalendarViewMode.Day;
        var day = vm.MonthYearDisplay;
        vm.ViewMode = FfiCalendarViewMode.Agenda;
        var agenda = vm.MonthYearDisplay;

        Assert.Equal(4, new[] { month, week, day, agenda }.Distinct().Count());
        // The day label names the day; the month label cannot.
        Assert.Contains("17", day);
        Assert.DoesNotContain("17", month);
        // Agenda is date-unfiltered — it names the list, so it carries no year.
        Assert.DoesNotContain("2026", agenda);
    }

    /// <summary>The pan policy: one visible range per click, in whatever mode is
    /// showing. The DISTANCE comes from the shared <c>calendar_pan_step</c>; only
    /// the sign and the walk are windows'.
    ///
    /// <para>Asserted through the anchor rather than the label, because the label
    /// is a formatted string and this is about the date arithmetic. Both anchors
    /// are checked: they move in lockstep, which is what stops a pan in month view
    /// leaving the week grid on a pre-pan day.</para></summary>
    /// Resolve a wire word to the shared mode the way an app would if it ever
    /// needed to — `calendar_view_modes()` + `calendar_view_mode_wire`, the pair
    /// that makes `CalendarViewMode::from_wire` unnecessary on a native.
    /// (Also why these theories take a string: xUnit needs public test methods,
    /// and a public method cannot take the UniFFI-internal enum.)
    private static FfiCalendarViewMode ModeFromWire(string wire) =>
        FaunaFfiMethods.CalendarViewModes()
            .First(m => FaunaFfiMethods.CalendarViewModeWire(m) == wire);

    [Theory]
    [InlineData("month", 1, "2026-04-17")]
    [InlineData("month", -1, "2026-02-17")]
    [InlineData("week", 1, "2026-03-24")]
    [InlineData("week", -1, "2026-03-10")]
    [InlineData("day", 1, "2026-03-18")]
    [InlineData("day", -1, "2026-03-16")]
    public async Task Navigate_MovesOneVisibleRange_PerViewMode(
        string modeWire, int direction, string expected)
    {
        var mode = ModeFromWire(modeWire);
        var anchor = new DateTimeOffset(2026, 3, 17, 0, 0, 0, TimeSpan.Zero);
        var vm = new EventsViewModel(new MockNestRpcClient())
        {
            CurrentMonth = anchor,
            CurrentDay = anchor,
            ViewMode = mode,
        };

        await vm.NavigateCommand.ExecuteAsync(direction);

        Assert.Equal(expected, vm.CurrentDay.ToString("yyyy-MM-dd"));
        Assert.Equal(expected, vm.CurrentMonth.ToString("yyyy-MM-dd"));
    }

    /// <summary>⚠ <c>PanStep::None</c> is "do nothing", NOT "move by zero".
    ///
    /// <para>The agenda list is date-unfiltered, so it ignores the anchor — a
    /// click that quietly moved it would change state nothing renders, and then
    /// show a range the user never navigated to on their next mode switch. The
    /// tui slice found exactly that mutant (anchor moved a week, label above it
    /// unchanged, assertion still passed), which is why this asserts the ANCHOR
    /// and not the label.</para></summary>
    [Fact]
    public async Task Navigate_InAgenda_MovesNothingAtAll()
    {
        var anchor = new DateTimeOffset(2026, 3, 17, 0, 0, 0, TimeSpan.Zero);
        var vm = new EventsViewModel(new MockNestRpcClient())
        {
            CurrentMonth = anchor,
            CurrentDay = anchor,
            ViewMode = FfiCalendarViewMode.Agenda,
        };
        var labelBefore = vm.MonthYearDisplay;

        await vm.NavigateCommand.ExecuteAsync(1);
        await vm.NavigateCommand.ExecuteAsync(-1);

        Assert.Equal(anchor, vm.CurrentDay);
        Assert.Equal(anchor, vm.CurrentMonth);
        Assert.Equal(labelBefore, vm.MonthYearDisplay);
    }

    /// <summary>The wire word comes from the shared vocabulary, not a C# switch
    /// that could drift from it (<c>events.md</c> — `as_wire` is "the one spelling
    /// every app's toggle, automation surface and persisted view state uses").</summary>
    [Theory]
    [InlineData("agenda")]
    [InlineData("month")]
    [InlineData("week")]
    [InlineData("day")]
    public void ViewModeWire_ReadsTheSharedVocabulary(string expected)
    {
        var vm = new EventsViewModel(new MockNestRpcClient()) { ViewMode = ModeFromWire(expected) };

        Assert.Equal(expected, vm.ViewModeWire);
    }

    [Fact]
    public async Task Rsvp_CallsRpc_WithTypedResponse()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventsViewModel(rpc);

        // RsvpAsync forwards the hex id verbatim (no byte decode) and parses the
        // status into the typed RsvpResponse submission set.
        await vm.RsvpCommand.ExecuteAsync("abcd:declined");

        Assert.Contains("CaldavRsvpEvent", rpc.Calls);
        Assert.Equal(uniffi.fauna_core.RsvpResponse.Declined, rpc.LastRsvpResponse);
    }

    [Fact]
    public async Task CreateEvent_CallsRpc_WithSelectedCalendarHexId()
    {
        // CreateEventAsync needs a calendar (create_event requires the calendar
        // id), so load the "Work" calendar fixture first, then create.
        var rpc = CreateRpcWithCalendars();
        var vm = new EventsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        var draft = new EventDraft("Test", "2026-03-28T10:00:00Z", "2026-03-28T11:00:00Z", null, null);
        await vm.CreateEventCommand.ExecuteAsync(draft);

        Assert.Contains("CaldavCreateEvent", rpc.Calls);
        Assert.Equal(("c1", "Test"), rpc.LastCreatedEvent);
    }

    [Fact]
    public async Task CreateCalendar_CallsRpc()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventsViewModel(rpc);

        await vm.CreateCalendarCommand.ExecuteAsync("Personal");

        Assert.Contains("CaldavCreateCalendar", rpc.Calls);
    }

    /// <summary>
    /// The stale-query-overwrites-narrowed-selection race (events.md § Implementation
    /// status today, the no-selection-union row; apple's <c>4e5dac926</c>, mirrored by
    /// android/web/tui): creating a calendar kicks off a SLOW no-selection union
    /// (fans out over every owned calendar); tapping the new calendar immediately after
    /// fires a second, independent, FAST single-calendar query concurrently, through
    /// <see cref="EventsViewModel.QueryEventsAsync"/>'s one choke point (every caller —
    /// <c>LoadCommand</c>, <c>NavigateCommand</c>, week/day nav — routes through it,
    /// so the second trigger here is <c>NavigateCommand</c> rather than a second
    /// <c>LoadCommand</c>, deliberately isolating the events-generation race from the
    /// UNRELATED <c>Calendars</c>-collection re-populate two concurrent <c>LoadAsync</c>
    /// calls would also race — that is a real but separate concern, not this guard's job).
    /// Reproduced deterministically — gate the union's first fan-out call ("c1") mid-flight,
    /// let the second, differently-scoped query complete fully first, THEN release the gate
    /// — and assert the stale union does NOT clobber the fresher, narrower result on landing.
    /// </summary>
    [Fact]
    public async Task Load_StaleUnionAfterFasterNarrowerSelection_DoesNotClobberIt()
    {
        var slowGate = new TaskCompletionSource<bool>();
        var rpc = new MockNestRpcClient
        {
            NextCaldavCalendars = new List<FfiCalendarRow>
            {
                MockNestRpcClient.MakeCaldavCalendar("c1", "Work"),
                MockNestRpcClient.MakeCaldavCalendar("c2", "Home"),
                // Present from the start — mirrors the real flow, where
                // CreateCalendarAsync's LoadAsync refreshes Calendars (picking up
                // the new one) BEFORE it fires the events query that races here.
                // resolve_calendar_selection (track 6) treats a ghost id — one
                // never in Calendars — as vanished and falls back to the union, so
                // "fast" must be a real member or this test would race track 6's
                // fix instead of the stale-generation guard it targets.
                MockNestRpcClient.MakeCaldavCalendar("fast", "Fast"),
            },
            CaldavQueryEventsById = new Dictionary<string, IReadOnlyList<FfiCalEvent>>
            {
                ["c1"] = new List<FfiCalEvent> { MockNestRpcClient.MakeCaldavEvent("u1", "Union1", "2026-03-28T10:00:00Z") },
                ["c2"] = new List<FfiCalEvent> { MockNestRpcClient.MakeCaldavEvent("u2", "Union2", "2026-03-28T11:00:00Z") },
                ["fast"] = new List<FfiCalEvent> { MockNestRpcClient.MakeCaldavEvent("f1", "Fast", "2026-03-28T12:00:00Z") },
            },
        };
        var vm = new EventsViewModel(rpc);
        // Agenda's pan is a no-op by policy (PanStep::None), so a test using a pan
        // as its re-query trigger must sit in a mode that HAS a range.
        vm.ViewMode = FfiCalendarViewMode.Month;
        // Populate Calendars once, up front — neither trigger below re-populates it, so
        // the union's foreach never races a concurrent Clear()/Add() from the fast path.
        // The gate is armed only AFTER this call: LoadAsync's own trailing QueryEventsAsync
        // also unions over c1/c2/fast (SelectedCalendar is still null here), so an
        // already-armed gate would deadlock this very call — nothing left to release it.
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.Equal(3, vm.Calendars.Count); // sanity: c1/c2/fast loaded before the race starts

        rpc.CaldavQueryEventsHook = id => id == "c1" ? slowGate.Task : Task.CompletedTask;

        // The slow no-selection union (mirrors CreateCalendarAsync's own LoadAsync):
        // SelectedCalendar is still null, so this fans out over Calendars; runs
        // synchronously up to the gated "c1" fan-out call, then suspends.
        var slowLoad = vm.NavigateCommand.ExecuteAsync(0);

        // The fast, narrower query (mirrors CalendarList_SelectionChanged tapping the
        // just-created calendar) — ungated, runs to completion first.
        vm.SelectedCalendar = new CalendarInfo("fast", "Fast", "", null);
        await vm.NavigateCommand.ExecuteAsync(0);

        Assert.Single(vm.Events);
        Assert.Equal("f1", vm.Events[0].Id);

        // Release the stale union; it must NOT land on top of the fresher selection.
        slowGate.SetResult(true);
        await slowLoad;

        Assert.Single(vm.Events);
        Assert.Equal("f1", vm.Events[0].Id);
    }

    /// <summary>
    /// events.md § Where logic lives → "Which calendars the page is scoped to": a
    /// selection naming a calendar that no longer exists — deleted here, or by an
    /// external CalDAV MUA against the same <c>bridge_caldav_*</c> store — must fall
    /// back to the union rather than querying the gone id and reading empty with no
    /// error. Resolved at READ time through the shared
    /// <c>fauna_client_caldav::resolve_calendar_selection</c> UniFFI face; mirrors
    /// android's <c>EventsScopeTest.vanishedSelection_fallsBackToUnion</c>.
    /// </summary>
    [Fact]
    public async Task Load_SelectionNamingDeletedCalendar_FallsBackToUnion()
    {
        var rpc = new MockNestRpcClient
        {
            NextCaldavCalendars = new List<FfiCalendarRow>
            {
                MockNestRpcClient.MakeCaldavCalendar("c1", "Work"),
            },
            CaldavQueryEventsById = new Dictionary<string, IReadOnlyList<FfiCalEvent>>
            {
                ["c1"] = new List<FfiCalEvent> { MockNestRpcClient.MakeCaldavEvent("u1", "Union1", "2026-03-28T10:00:00Z") },
            },
        };
        var vm = new EventsViewModel(rpc);
        // Agenda's pan is a no-op by policy (PanStep::None), so a test using a pan
        // as its re-query trigger must sit in a mode that HAS a range.
        vm.ViewMode = FfiCalendarViewMode.Month;
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.Single(vm.Calendars);

        // "gone" was never in Calendars — simulates a selection surviving the
        // deletion of its own calendar (LoadAsync never clears SelectedCalendar).
        vm.SelectedCalendar = new CalendarInfo("gone", "Gone", "", null);
        await vm.NavigateCommand.ExecuteAsync(0);

        Assert.Single(vm.Events);
        Assert.Equal("u1", vm.Events[0].Id);
    }

    // ── calendar-visibility display filter (events.md § Where logic lives →
    // Which calendars display, ratified 2026-08-02) — the shared
    // fauna_client_caldav::calendar_is_displayed/resolve_calendar_selection
    // composition, consumed via UniFFI exactly like tui/linux/web/android/apple.
    // These pin the two edge cases that drifted apart across the pre-lift
    // per-app copies (events.md's own history): the empty-visible-set-means-union
    // rule, and seeding a brand-new calendar visible on every path that replaces
    // Calendars — mirroring apple's EventsVMCalendarVisibilityTests.swift.

    private static MockNestRpcClient CreateRpcWithTwoCalendars() => new MockNestRpcClient
    {
        NextCaldavCalendars = new List<FfiCalendarRow>
        {
            MockNestRpcClient.MakeCaldavCalendar("c1", "One"),
            MockNestRpcClient.MakeCaldavCalendar("c2", "Two"),
        },
        CaldavQueryEventsById = new Dictionary<string, IReadOnlyList<FfiCalEvent>>
        {
            ["c1"] = new List<FfiCalEvent> { MockNestRpcClient.MakeCaldavEvent("ev-a", "EventA", "2026-08-07T14:00:00Z", calendarIdHex: "c1") },
            ["c2"] = new List<FfiCalEvent> { MockNestRpcClient.MakeCaldavEvent("ev-b", "EventB", "2026-08-07T15:00:00Z", calendarIdHex: "c2") },
        },
    };

    [Fact]
    public async Task Load_SeedsEveryCalendarVisible_OnFirstLoad()
    {
        var vm = new EventsViewModel(CreateRpcWithTwoCalendars());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(new[] { "c1", "c2" }, vm.VisibleCalendarIds.OrderBy(x => x));
        Assert.Equal(2, vm.CalendarRows.Count);
        Assert.All(vm.CalendarRows, r => Assert.True(r.IsVisible));
        Assert.Equal(new[] { "ev-a", "ev-b" }, vm.Events.Select(e => e.Id).OrderBy(x => x));
    }

    [Fact]
    public async Task Toggle_HidesExactlyThatCalendarsEvents()
    {
        var rpc = CreateRpcWithTwoCalendars();
        var vm = new EventsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.ToggleCalendarVisibility("c1");

        Assert.Equal(new[] { "ev-b" }, vm.Events.Select(e => e.Id));
        Assert.DoesNotContain("c1", vm.VisibleCalendarIds);
        Assert.Contains(vm.CalendarRows, r => r.Calendar.Id == "c1" && !r.IsVisible);
        Assert.Contains(vm.CalendarRows, r => r.Calendar.Id == "c2" && r.IsVisible);
    }

    [Fact]
    public async Task Toggle_UncheckingEveryBox_EmptiesTheSet_WhichIsTheFullUnion()
    {
        var vm = new EventsViewModel(CreateRpcWithTwoCalendars());
        await vm.LoadCommand.ExecuteAsync(null);

        vm.ToggleCalendarVisibility("c1");
        vm.ToggleCalendarVisibility("c2");

        Assert.Empty(vm.VisibleCalendarIds);
        Assert.Equal(new[] { "ev-a", "ev-b" }, vm.Events.Select(e => e.Id).OrderBy(x => x));
    }

    [Fact]
    public async Task Toggle_DoesNotRefetch_ReFiltersTheAlreadyFetchedUnion()
    {
        var rpc = CreateRpcWithTwoCalendars();
        var vm = new EventsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        var callsAfterLoad = rpc.Calls.Count;

        vm.ToggleCalendarVisibility("c1");
        vm.ToggleCalendarVisibility("c1");

        Assert.Equal(callsAfterLoad, rpc.Calls.Count);
    }

    [Fact]
    public async Task Toggle_ANewCalendarAppears_StaysVisible_WhileAnExistingOffChoiceSurvives()
    {
        var rpc = CreateRpcWithTwoCalendars();
        var vm = new EventsViewModel(rpc);
        // Agenda's pan is a no-op by policy (PanStep::None), so a test using a pan
        // as its re-query trigger must sit in a mode that HAS a range.
        vm.ViewMode = FfiCalendarViewMode.Month;
        await vm.LoadCommand.ExecuteAsync(null);
        vm.ToggleCalendarVisibility("c1"); // user hides c1

        // A brand-new calendar (c3) appears on the next load.
        rpc.NextCaldavCalendars = new List<FfiCalendarRow>
        {
            MockNestRpcClient.MakeCaldavCalendar("c1", "One"),
            MockNestRpcClient.MakeCaldavCalendar("c2", "Two"),
            MockNestRpcClient.MakeCaldavCalendar("c3", "Three"),
        };
        rpc.CaldavQueryEventsById!["c3"] = new List<FfiCalEvent>
        {
            MockNestRpcClient.MakeCaldavEvent("ev-c", "EventC", "2026-08-09T10:00:00Z", calendarIdHex: "c3"),
        };
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(new[] { "c2", "c3" }, vm.VisibleCalendarIds.OrderBy(x => x));
        Assert.Equal(new[] { "ev-b", "ev-c" }, vm.Events.Select(e => e.Id).OrderBy(x => x));
    }

    [Fact]
    public async Task Toggle_LiveSelectionIgnoresVisibility_SelectingWinsOutright()
    {
        var vm = new EventsViewModel(CreateRpcWithTwoCalendars());
        // Agenda's pan is a no-op by policy (PanStep::None), so a test using a pan
        // as its re-query trigger must sit in a mode that HAS a range.
        vm.ViewMode = FfiCalendarViewMode.Month;
        await vm.LoadCommand.ExecuteAsync(null);
        vm.SelectedCalendar = new CalendarInfo("c1", "One", "", null);
        await vm.NavigateCommand.ExecuteAsync(0);

        // Hiding the SELECTED calendar must not empty the page: selecting it
        // wins outright over the display filter.
        vm.ToggleCalendarVisibility("c1");

        Assert.Equal(new[] { "ev-a" }, vm.Events.Select(e => e.Id));
    }

    /// <summary>
    /// windows is the one app that unions the invited-events feed into the same
    /// no-selection list (<see cref="EventsViewModel.QueryEventItemsAsync"/>)
    /// instead of a separate always-visible section the way apple/android/tui
    /// do — so calendar-visibility (a MY-calendars concept) must never reach an
    /// event whose calendar the actor doesn't own, or an invited event would
    /// vanish the moment any calendar exists (seeding makes the visible set
    /// non-empty on the very first load).
    /// </summary>
    [Fact]
    public async Task Toggle_NeverHidesInvitedEvents_TheyAreNotOneOfMyCalendars()
    {
        var rpc = CreateRpcWithTwoCalendars();
        rpc.NextCaldavInvited = new List<FfiCalEvent>
        {
            MockNestRpcClient.MakeCaldavEvent(
                "ev-invited", "Invited", "2026-08-10T09:00:00Z", calendarIdHex: "someone-elses-calendar"),
        };
        var vm = new EventsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        // Hide every owned calendar — the invited event must still show.
        vm.ToggleCalendarVisibility("c1");
        vm.ToggleCalendarVisibility("c2");

        Assert.Contains(vm.Events, e => e.Id == "ev-invited");
    }

    [Fact]
    public void SeedVisibleCalendarIds_EmptyPrevious_SeedsEveryCalendar()
    {
        var next = new List<CalendarInfo> { new("c1", "One", "", null), new("c2", "Two", "", null) };

        var seeded = EventsViewModel.SeedVisibleCalendarIds(new HashSet<string>(), new List<CalendarInfo>(), next);

        Assert.Equal(new[] { "c1", "c2" }, seeded.OrderBy(x => x));
    }

    [Fact]
    public void SeedVisibleCalendarIds_ExistingChoiceSurvives_NewCalendarSeededVisible()
    {
        var previous = new List<CalendarInfo> { new("c1", "One", "", null), new("c2", "Two", "", null) };
        var next = new List<CalendarInfo>
        {
            new("c1", "One", "", null), new("c2", "Two", "", null), new("c3", "Three", "", null),
        };
        var visible = new HashSet<string> { "c2" }; // user had hidden c1

        var seeded = EventsViewModel.SeedVisibleCalendarIds(visible, previous, next);

        Assert.Equal(new[] { "c2", "c3" }, seeded.OrderBy(x => x));
    }
}

// ── EventDetailViewModel Tests ──

public class EventDetailViewModelTests
{
    [Fact]
    public async Task Load_PopulatesFromSingleGetEvent_NoSeparateFetches()
    {
        var rpc = new MockNestRpcClient
        {
            NextCaldavEvent = MockNestRpcClient.MakeCaldavEvent(
                "abcd", "Standup", "2026-03-28T10:00:00Z", "2026-03-28T10:30:00Z",
                organizedByMe: true, reminder: "PT15M",
                attendees: new[]
                {
                    MockNestRpcClient.MakeCaldavAttendee("guest@example.com", "going", "Guest"),
                }),
        };
        var vm = new EventDetailViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync("abcd");

        // The single get_event carries summary + reminder + attendees + author flag.
        Assert.Contains("CaldavGetEvent", rpc.Calls);
        Assert.DoesNotContain("CaldavQueryEvents", rpc.Calls);
        Assert.Single(rpc.Calls);
        Assert.Equal("Standup", vm.Summary);
        Assert.Equal("PT15M", vm.ReminderOffset);
        Assert.True(vm.OrganizedByMe);
        Assert.Single(vm.Attendees);
        Assert.Equal("guest@example.com", vm.Attendees[0].Email);
        Assert.Equal("going", vm.Attendees[0].Rsvp);
    }

    [Fact]
    public async Task Load_ShowsError_WhenEventMissing()
    {
        var rpc = new MockNestRpcClient { NextCaldavEvent = null };
        var vm = new EventDetailViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync("abcd");

        Assert.Contains("CaldavGetEvent", rpc.Calls);
        Assert.NotNull(vm.ErrorMessage);
    }

    [Fact]
    public async Task Load_OrganizedByMe_ReflectsFakeEvent()
    {
        var rpc = new MockNestRpcClient
        {
            NextCaldavEvent = MockNestRpcClient.MakeCaldavEvent(
                "abcd", "Invited", "2026-03-28T10:00:00Z", organizedByMe: false),
        };
        var vm = new EventDetailViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync("abcd");

        Assert.False(vm.OrganizedByMe);
    }

    [Fact]
    public async Task Invite_CallsRpcWithEmail()
    {
        var rpc = new MockNestRpcClient
        {
            NextCaldavEvent = MockNestRpcClient.MakeCaldavEvent("abcd", "E", "2026-03-28T10:00:00Z"),
        };
        var vm = new EventDetailViewModel(rpc) { EventId = "abcd" };

        await vm.InviteCommand.ExecuteAsync("guest@example.com");

        Assert.Contains("CaldavInviteAttendee", rpc.Calls);
        Assert.Equal("guest@example.com", rpc.LastInviteEmail);
        // The detail re-loads after inviting (one get_event carries the new roster).
        Assert.Contains("CaldavGetEvent", rpc.Calls);
    }

    [Fact]
    public async Task Delete_CallsRpc()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventDetailViewModel(rpc) { EventId = "abcd" };

        await vm.DeleteCommand.ExecuteAsync(null);

        Assert.Contains("CaldavDeleteEvent", rpc.Calls);
    }

    [Fact]
    public async Task Invite_NoOp_WhenEventIdNull()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventDetailViewModel(rpc);

        await vm.InviteCommand.ExecuteAsync("guest@example.com");

        Assert.Empty(rpc.Calls);
    }
}

// ── encode_filter_rule (shared Rust FFI) — cross-language conformance ──
//
// FeedRuleEncoder.cs was lifted into the shared `libs/fauna-ffi`
// `encode_filter_rule` (refactor(rust,windows)). These exercise the REAL
// native FFI — the dotnet test host loads the fauna_ffi dll — so C# and Rust
// cannot drift on the wire shape. Semantics per docs/goal/ui/feed.md
// § Filter rule types: `CreatedAfter` input is in hours; label thresholds ride
// a 0–10 scale (× 100 → the per-mille u16 the dag-cbor wire carries).

public class FeedRuleEncodingFfiTests
{
    private static JsonElement Enc(string type, string value, bool required) =>
        JsonSerializer.Deserialize<JsonElement>(FaunaFfiMethods.EncodeFilterRule(type, value, required));

    [Fact]
    public void BodyContains_EmitsExternallyTaggedTerms()
    {
        var terms = Enc("BodyContains", "rust, svelte", false).GetProperty("BodyContains").GetProperty("terms");
        Assert.Equal(JsonValueKind.Array, terms.ValueKind);
        Assert.Equal("rust", terms[0].GetString());
        Assert.Equal("svelte", terms[1].GetString());
    }

    [Fact]
    public void HasHashtag_EmitsTagsArray()
    {
        var tags = Enc("HasHashtag", "photography", false).GetProperty("HasHashtag").GetProperty("tags");
        Assert.Equal("photography", tags[0].GetString());
    }

    [Fact]
    public void Source_SplitsCommaSeparatedProtocols()
    {
        var protocols = Enc("Source", "fauna, bluesky", false).GetProperty("Source").GetProperty("protocols");
        Assert.Equal("fauna", protocols[0].GetString());
        Assert.Equal("bluesky", protocols[1].GetString());
    }

    [Fact]
    public void HasMedia_EmitsRequiredBool()
    {
        Assert.True(Enc("HasMedia", "", true).GetProperty("HasMedia").GetProperty("required").GetBoolean());
    }

    [Fact]
    public void MinReplies_EmitsCountNumber()
    {
        Assert.Equal(5, Enc("MinReplies", "5", false).GetProperty("MinReplies").GetProperty("count").GetInt32());
    }

    [Fact]
    public void CreatedAfter_ConvertsHoursToMicroseconds()
    {
        // feed.md: input is hours. 24 h → 24 × 3.6e9 µs = 86_400_000_000.
        Assert.Equal(
            86_400_000_000L,
            Enc("CreatedAfter", "24", false).GetProperty("CreatedAfter").GetProperty("age_microseconds").GetInt64());
    }

    [Fact]
    public void LabelBelow_ConvertsThresholdOnZeroToTenScale()
    {
        // 0–10 scale: threshold 5 → 5 × 100 = 500 ‰.
        var label = Enc("LabelBelow", "spam:5", false).GetProperty("LabelBelow");
        Assert.Equal("spam", label.GetProperty("category").GetString());
        Assert.Equal(500, label.GetProperty("max_confidence_permille").GetInt32());
    }

    [Fact]
    public void LabelAbove_ConvertsThresholdToPermille()
    {
        var label = Enc("LabelAbove", "nsfw:8", false).GetProperty("LabelAbove");
        Assert.Equal(800, label.GetProperty("min_confidence_permille").GetInt32());
    }

    [Fact]
    public void UnknownRuleType_Throws()
    {
        // No silent "{}" misencode — the shared encoder rejects unknown types.
        Assert.ThrowsAny<System.Exception>(() => FaunaFfiMethods.EncodeFilterRule("Bogus", "x", false));
    }
}

// ── Shared port validator conformance — the single validator behind the admin
//    CalDAV- and serving-port fields (AdminCalendarViewModel.SaveCaldavPortAsync /
//    AdminNestViewModel.SaveServingPortAsync) is the shared
//    fauna_core::format::parse_port over the value-format FFI face
//    (FaunaFfiMethods.ParsePort → ushort?). These exercise the REAL native FFI dll
//    (no per-app [1, 65535] re-roll). See value-formatting.md § Port validation.

public class PortValidationFfiTests
{
    [Theory]
    [InlineData("1", (ushort)1)]          // lower bound
    [InlineData("443", (ushort)443)]
    [InlineData("8443", (ushort)8443)]
    [InlineData("65535", (ushort)65535)]  // upper bound
    [InlineData("  443  ", (ushort)443)]  // surrounding whitespace is trimmed
    [InlineData("+443", (ushort)443)]     // leading '+' is a valid unsigned literal
    public void ParsePort_AcceptsValidPorts(string input, ushort expected)
    {
        Assert.Equal((ushort?)expected, FaunaFfiMethods.ParsePort(input));
    }

    [Theory]
    [InlineData("0")]      // 0 is not a bindable listener port
    [InlineData("65536")]  // above u16 max
    [InlineData("99999")]  // far above range
    [InlineData("")]       // empty
    [InlineData("abc")]    // non-numeric
    [InlineData("-1")]     // negative
    [InlineData("84.43")]  // fractional
    [InlineData("8 443")]  // interior whitespace
    public void ParsePort_RejectsInvalidPorts(string input)
    {
        Assert.Null(FaunaFfiMethods.ParsePort(input));
    }
}

// ── Shared mail-knob integer validator conformance — the single validators behind the
//    AdminMail page's numeric knob fields (AdminMailPage.ParseUint / ParseUlong, ~17 u32
//    knobs + the u64 IMAP storage ceiling) are the shared fauna_core::format::parse_count
//    / parse_count_u64 over the value-format FFI face (FaunaFfiMethods.ParseCount → uint?,
//    ParseCountU64 → ulong?). These exercise the REAL native FFI dll (no per-app
//    TryParse re-roll). See value-formatting.md § Mail-knob validation.

public class MailKnobValidationFfiTests
{
    [Theory]
    [InlineData("0", (uint)0)]                 // 0 is a valid knob value (unlike a port)
    [InlineData("3600", (uint)3600)]
    [InlineData("4294967295", uint.MaxValue)]  // u32 max round-trips
    [InlineData("  42  ", (uint)42)]           // surrounding whitespace is trimmed
    [InlineData("+7", (uint)7)]                // leading '+' is a valid unsigned literal
    public void ParseCount_AcceptsValid(string input, uint expected)
    {
        Assert.Equal((uint?)expected, FaunaFfiMethods.ParseCount(input));
    }

    [Theory]
    [InlineData("")]            // empty
    [InlineData("   ")]         // whitespace only
    [InlineData("abc")]         // non-numeric
    [InlineData("-1")]          // negative (an unsigned knob)
    [InlineData("1.5")]         // fractional
    [InlineData("4294967296")]  // above u32 max (overflow)
    [InlineData("4 2")]         // interior whitespace
    public void ParseCount_RejectsInvalid(string input)
    {
        Assert.Null(FaunaFfiMethods.ParseCount(input));
    }

    [Theory]
    [InlineData("0", (ulong)0)]
    [InlineData("107374182400", (ulong)107374182400)]     // a 100 GiB storage ceiling
    [InlineData("18446744073709551615", ulong.MaxValue)]  // u64 max round-trips
    [InlineData("  100  ", (ulong)100)]                   // surrounding whitespace trimmed
    public void ParseCountU64_AcceptsValid(string input, ulong expected)
    {
        Assert.Equal((ulong?)expected, FaunaFfiMethods.ParseCountU64(input));
    }

    [Theory]
    [InlineData("")]
    [InlineData("nope")]
    [InlineData("-5")]
    [InlineData("3.14")]
    [InlineData("18446744073709551616")]  // above u64 max (overflow)
    public void ParseCountU64_RejectsInvalid(string input)
    {
        Assert.Null(FaunaFfiMethods.ParseCountU64(input));
    }
}

// ── Shared per-alias rate-limit override validator conformance — the mail-alias add-sheet's
//    rate_limit_per_hour (signed i64; null = unlimited, 0 = block all) parses via the shared
//    fauna_core::format::parse_count_i64 over the value-format FFI face
//    (FaunaFfiMethods.ParseCountI64 → long?). Exercises the REAL native FFI dll (no per-app
//    Int64/TryParse re-roll, which accepted a negative — a negative rate_limit_per_hour reaching
//    the nest 451-tempfails all alias mail). See value-formatting.md § Per-alias rate-cap validation.

public class RateLimitOverrideFfiTests
{
    [Theory]
    [InlineData("0", 0L)]                               // explicit "block all"
    [InlineData("100", 100L)]
    [InlineData("9223372036854775807", long.MaxValue)]  // i64 max round-trips
    [InlineData("  500  ", 500L)]                        // surrounding whitespace trimmed
    [InlineData("+42", 42L)]                             // leading '+' is a valid signed literal
    public void ParseCountI64_AcceptsNonNegative(string input, long expected)
    {
        Assert.Equal((long?)expected, FaunaFfiMethods.ParseCountI64(input));
    }

    [Theory]
    [InlineData("")]                       // empty → no override
    [InlineData("   ")]                    // whitespace only
    [InlineData("abc")]                    // non-numeric
    [InlineData("-5")]                     // negative rejected (would 451-tempfail all alias mail)
    [InlineData("1.5")]                    // fractional
    [InlineData("9223372036854775808")]    // i64::MAX + 1 overflow
    [InlineData("8 443")]                  // interior whitespace
    public void ParseCountI64_RejectsInvalidOrNegative(string input)
    {
        Assert.Null(FaunaFfiMethods.ParseCountI64(input));
    }
}

// ── Shared spam-threshold probability↔per-mille conversion conformance — the spam/phishing
//    slider (0.0–1.0) ↔ the per-mille u16 wire goes through the shared
//    fauna_protocol::spam::{probability_to_per_mille,per_mille_to_probability} over the
//    value-format FFI face (FaunaFfiMethods.{ProbabilityToPerMille,PerMilleToProbability}).
//    The shared fn rounds half-AWAY-from-zero (the nest/web/linux/android rule); C#'s Math.Round
//    defaults to banker's rounding, which this single-source replaces. See settings.md § Spam
//    threshold slider labels.

public class SpamThresholdConversionFfiTests
{
    [Theory]
    [InlineData(0.0, (ushort)0)]
    [InlineData(0.5, (ushort)500)]
    [InlineData(1.0, (ushort)1000)]
    [InlineData(0.0005, (ushort)1)]    // 0.5 per-mille → 1 (half away from zero; C# banker's gives 0)
    [InlineData(0.4567, (ushort)457)]
    [InlineData(-0.3, (ushort)0)]      // clamped below [0,1]
    [InlineData(1.7, (ushort)1000)]    // clamped above [0,1]
    public void ProbabilityToPerMille_ClampsAndRoundsHalfAway(double probability, ushort expected)
    {
        Assert.Equal(expected, FaunaFfiMethods.ProbabilityToPerMille(probability));
    }

    [Theory]
    [InlineData((ushort)0, 0.0)]
    [InlineData((ushort)250, 0.25)]
    [InlineData((ushort)500, 0.5)]
    [InlineData((ushort)1000, 1.0)]
    public void PerMilleToProbability_IsTheInverse(ushort perMille, double expected)
    {
        Assert.Equal(expected, FaunaFfiMethods.PerMilleToProbability(perMille));
    }
}

// ── BackupsViewModel Tests ──
//
// The SNAPSHOT half runs over the shared `fauna-backups-machine`, faked here
// through its generated `IBackupsMachine` interface (machine-as-seam). These
// tests pin what stays the CLIENT's job after the adoption — the projection, the
// gesture wiring and the two client-glue affordances (the delete confirm's
// dispatch and the friction-bar modal's close rule). Everything they used to pin
// about page LOGIC (selection default, single-flight, prune policy, the check
// verdict) now lives in the machine's own 41 tier_1 tests, and the per-call RPC
// arguments they asserted no longer exist.
//
// The destination + restore halves below still run over MockNestRpcClient.

/// A hand-driven `IBackupsMachine`: the tests set `Current` (the snapshot the VM
/// projects) and read back which gestures fired. Derives from the generated
/// `BackupsMachineFakeBase`, so every un-overridden member throws rather than
/// silently answering — an accidental new call site fails loudly.
internal sealed class FakeBackupsMachine : uniffi.fauna_backups_machine.BackupsMachineFakeBase
{
    public uniffi.fauna_backups_machine.BackupsSnapshot Current =
        BackupsTestData.Snapshot();

    public readonly List<string> Calls = new();
    public long? LastDeleted;
    public long? LastUndeleted;
    public (long Id, string Confirm, string Ack)? LastImmediateDelete;
    public string? LastSelectedFolder;

    /// What `ImmediateDeleteEnabled` answers. Deliberately independent of the
    /// inputs, so a VM that re-derived the predicate locally instead of CALLING
    /// the machine would disagree with it and fail the test.
    public bool ImmediateDeleteAnswer;

    public override uniffi.fauna_backups_machine.BackupsSnapshot Snapshot() => Current;

    public override Task Refresh() { Calls.Add("Refresh"); return Task.CompletedTask; }

    public override Task SelectFolder(string name)
    {
        Calls.Add("SelectFolder");
        LastSelectedFolder = name;
        return Task.CompletedTask;
    }

    public override Task CreateSnapshot() { Calls.Add("CreateSnapshot"); return Task.CompletedTask; }

    public override Task DeleteSnapshot(long snapshotId)
    {
        Calls.Add("DeleteSnapshot");
        LastDeleted = snapshotId;
        return Task.CompletedTask;
    }

    public override Task UndeleteSnapshot(long snapshotId)
    {
        Calls.Add("UndeleteSnapshot");
        LastUndeleted = snapshotId;
        return Task.CompletedTask;
    }

    public override Task DeleteSnapshotImmediate(long snapshotId, string confirmId, string acknowledge)
    {
        Calls.Add("DeleteSnapshotImmediate");
        LastImmediateDelete = (snapshotId, confirmId, acknowledge);
        return Task.CompletedTask;
    }

    public override bool ImmediateDeleteEnabled(string confirmId, string targetId, string acknowledge)
    {
        Calls.Add("ImmediateDeleteEnabled");
        return ImmediateDeleteAnswer;
    }

    public override Task PrunePreview() { Calls.Add("PrunePreview"); return Task.CompletedTask; }
    public override Task PruneExecute() { Calls.Add("PruneExecute"); return Task.CompletedTask; }
    public override void CancelPrunePreview() => Calls.Add("CancelPrunePreview");
    public override Task Check() { Calls.Add("Check"); return Task.CompletedTask; }
    public override Task OpenSnapshot(long snapshotId) { Calls.Add("OpenSnapshot"); return Task.CompletedTask; }
    public override void CloseSnapshotDetail() => Calls.Add("CloseSnapshotDetail");
}

/// Builders for the machine's renderable records (all-positional UniFFI records,
/// so a named helper keeps the tests readable).
internal static class BackupsTestData
{
    public static uniffi.fauna_backups_machine.BackupsSnapshot Snapshot(
        uniffi.fauna_backups_machine.BackupFolderRow[]? folders = null,
        string? selected = null,
        uniffi.fauna_backups_machine.SnapshotRow[]? rows = null,
        long? lastBackedUp = null,
        uniffi.fauna_backups_machine.BackupOp? op = null,
        uniffi.fauna_backups_machine.CheckOutcome? check = null,
        uniffi.fauna_backups_machine.PrunePreview? preview = null,
        uniffi.fauna_backups_machine.SnapshotDetail? detail = null,
        uniffi.fauna_core.LocalizedText? error = null) =>
        new(folders ?? Array.Empty<uniffi.fauna_backups_machine.BackupFolderRow>(),
            selected,
            rows ?? Array.Empty<uniffi.fauna_backups_machine.SnapshotRow>(),
            lastBackedUp, op, check, preview, detail, error);

    public static uniffi.fauna_backups_machine.BackupFolderRow Folder(
        string name, long count = 0, long? last = null) =>
        new(name, count, last);

    public static uniffi.fauna_backups_machine.SnapshotRow Row(
        long id, long createdAt = 1_752_000_000, long files = 3, long bytes = 4096,
        uniffi.fauna_backups_machine.SnapshotState? state = null,
        uniffi.fauna_backups_machine.RowIntegrity integrity =
            uniffi.fauna_backups_machine.RowIntegrity.Unknown) =>
        new(id, createdAt, files, bytes, null, Array.Empty<string>(),
            state ?? new uniffi.fauna_backups_machine.SnapshotState.Active(), integrity);

    public static uniffi.fauna_backups_machine.SnapshotDetail Detail(
        long id, params uniffi.fauna_backups_machine.SnapshotFileRow[] files) =>
        new(id, files);

    public static uniffi.fauna_backups_machine.SnapshotFileRow File(
        string path, long size = 512, string fileType = "regular") =>
        new(path, size, fileType, "00");

    public static uniffi.fauna_backups_machine.CheckOutcome Check(
        bool ok, long missingManifests = 0, long missingChunks = 0, long corrupt = 0,
        long[]? implicated = null) =>
        new(ok, 1, 2, 3, 4, missingManifests, missingChunks, corrupt,
            implicated ?? Array.Empty<long>());

    public static uniffi.fauna_backups_machine.PrunePreview Preview(
        uniffi.fauna_backups_machine.PolicyState state,
        long wouldPrune = 0, long remaining = 0,
        uniffi.fauna_backups_machine.PruneCandidate[]? candidates = null) =>
        new(wouldPrune, remaining,
            candidates ?? Array.Empty<uniffi.fauna_backups_machine.PruneCandidate>(), state);
}

/// Serializes with the other <c>Strings.Initialize</c>-mutating classes (see
/// <c>StringsGlobalCollection</c>): the snapshot-half assertions read the
/// USER-VISIBLE text, which needs the real templates installed.
[Collection("StringsGlobal")]
public class BackupsViewModelTests
{
    /// The backups + size templates the snapshot half renders, copied from
    /// <c>i18n/strings/en.yaml</c> verbatim (the windows resw keeps source
    /// <c>{name}</c> placeholders intact). Asserting resolved text rather than a
    /// bare key is the point: a row that rendered the raw key would satisfy every
    /// "contains a value" check while showing the user nothing.
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["backups/snapshot_row"] = "#{id} · {when} · {files} · {size}",
            ["backups/file_count"] = "{count} files",
            ["backups/last_backed_up_never"] = "Last backed up: never",
            ["backups/last_backed_up_at"] = "Last backed up: {when}",
            ["backups/snapshot_state_deletion_pending"] = "Deletion scheduled — cancel before {when}",
            ["backups/snapshot_state_deletion_pending_undated"] = "Deletion scheduled",
            ["backups/snapshot_state_soft_deleted"] = "Deleted — recoverable until {when}",
            ["backups/snapshot_state_soft_deleted_undated"] = "Deleted — still recoverable",
            ["backups/snapshot_integrity_ok"] = "integrity verified",
            ["backups/snapshot_integrity_implicated"] = "integrity problem found in this snapshot",
            ["backups/check_result_ok"] =
                "Integrity check passed — {snapshots} snapshots, {files} files, {chunks} chunks verified.",
            ["backups/check_result_errors"] =
                "Integrity check found problems: {missing_manifests} missing manifests, "
                + "{missing_chunks} missing chunks, {corrupt_manifests} corrupt manifests.",
            ["backups/prune_preview_counts"] = "{would_prune} would be deleted, {remaining} kept.",
            ["backups/prune_preview_candidate"] = "#{id} · {when}",
            ["backups/prune_preview_nothing"] =
                "Nothing to prune — every snapshot is within this set's retention policy.",
            ["backups/prune_policy_not_set"] =
                "No retention policy configured for this set. Set one on the Folders page.",
            ["backups/busy_create"] = "Creating a snapshot…",
            ["backups/busy_undelete"] = "Recovering a snapshot…",
            ["backups/busy_refresh"] = "Loading…",
            ["errors/network"] = "Network error",
            // ValueFormat.ByteSize resolves through the shared size.* keys.
            ["size/bytes"] = "{value} B",
            ["size/kb"] = "{value} KB",
            ["size/mb"] = "{value} MB",
            // Reclaim this device's copy — verbatim from i18n/strings/en.yaml, so
            // the assertions below read the SENTENCE a user sees rather than a raw
            // key (a key would satisfy every "not empty" check while saying
            // nothing).
            ["backups/backup_orphaned_store_row"] =
                "This device is still holding {held} of a backup copy. No destination uses it any more.",
            ["backups/backup_reclaim_still_hosting"] =
                "This device is still backing up right now, so nothing was deleted. Try again in a moment.",
            ["backups/backup_reclaim_after_remove_failed"] =
                "The destination was removed, but this device's copy could not be deleted: {reason}",
            ["backups/backup_reclaim_no_agent"] =
                "This device is not running a sync agent, so it holds no backup copy to delete.",
            ["backups/backup_reseed_running"] = "Restoring your data to this nest…",
            ["backups/backup_reseed_no_agent"] =
                "This device runs no backup service, so it holds no copy to restore from.",
            ["backups/backup_reseed_failed"] =
                "The restore stopped before anything was made live: {reason}",
            ["backups/backup_reseed_reenroll_failed"] =
                "Your data is back on this nest, but this device could not sign up again as its "
                + "backup: {reason}. Add this device as a backup destination to keep a copy here.",
            ["backups/reseed_result_whole"] = "Your data is back on this nest.",
            ["backups/reseed_set_mail"] = "Mail",
            ["backups/reseed_set_restored"] = "{set}: {count} restored",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public BackupsViewModelTests() => Strings.Initialize(new FakeLocalizer());

    /// A VM with the snapshot half attached to `machine`. The rpc seam is still
    /// needed for the download's byte fetch and the destination/restore halves.
    private static BackupsViewModel WithMachine(
        FakeBackupsMachine machine, MockNestRpcClient? rpc = null,
        string deviceId = "", ISnapshotFileSaver? saver = null)
    {
        var vm = new BackupsViewModel(rpc ?? new MockNestRpcClient(), deviceId: deviceId, fileSaver: saver);
        vm.AttachMachine(machine);
        return vm;
    }

    [Fact]
    public async Task Load_ProjectsSelectorAndRows_OffTheMachine()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                folders: new[] { BackupsTestData.Folder("documents"), BackupsTestData.Folder("photos") },
                selected: "documents",
                rows: new[] { BackupsTestData.Row(7), BackupsTestData.Row(8) }),
        };
        var vm = WithMachine(machine);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Contains("Refresh", machine.Calls);
        Assert.Equal(new[] { "documents", "photos" }, vm.FolderNames);
        Assert.Equal("documents", vm.SelectedFolder);
        Assert.Equal(2, vm.Snapshots.Count);
        Assert.Equal(7, vm.Snapshots[0].Id);
        Assert.Null(vm.ErrorMessage);
    }

    /// The § Snapshot-list shape *Row content contract*: a row renders at least a
    /// formatted created-at, a file count and a formatted size — "never a raw
    /// record dump". windows was the app the contract names: it bound the
    /// `SnapshotInfo` record's default `ToString()`, so the row read
    /// "SnapshotInfo { Id = 7, FileCount = 3, TotalBytes = 4096, CreatedAt =
    /// 1752000000, … }" — a raw epoch, raw bytes and no i18n.
    [Fact]
    public void SnapshotRowText_RendersFormattedContent_NotARawRecordDump()
    {
        var text = BackupsViewModel.SnapshotRowText(
            BackupsTestData.Row(7, createdAt: 1_752_000_000, files: 3, bytes: 4096));

        Assert.DoesNotContain("SnapshotInfo", text);
        Assert.DoesNotContain("TotalBytes", text);
        // Formatted size, not a raw byte count; formatted count, not a bare number;
        // and a formatted date, not the epoch.
        Assert.Contains("4 KB", text);
        Assert.Contains("3 files", text);
        Assert.DoesNotContain("1752000000", text);
        Assert.Contains("#7", text);
    }

    /// A non-Active state renders ON the row with the deadline the user can still
    /// act on, and integrity is ABSENT until a check runs this session (Unknown
    /// paints nothing — on this page the word "unknown" would read as a finding).
    [Fact]
    public void SnapshotRowText_RendersLifecycleState_AndOnlyKnownIntegrity()
    {
        var active = BackupsViewModel.SnapshotRowText(BackupsTestData.Row(1));
        Assert.DoesNotContain("Deletion scheduled", active);
        Assert.DoesNotContain("integrity", active);

        var pending = BackupsViewModel.SnapshotRowText(BackupsTestData.Row(
            2, state: new uniffi.fauna_backups_machine.SnapshotState.DeletionPending(1_752_000_000)));
        Assert.Contains("Deletion scheduled", pending);

        // The deadline is Option on the wire — the STATE still renders without it.
        var undated = BackupsViewModel.SnapshotRowText(BackupsTestData.Row(
            3, state: new uniffi.fauna_backups_machine.SnapshotState.DeletionPending(null)));
        Assert.Contains("Deletion scheduled", undated);

        var implicated = BackupsViewModel.SnapshotRowText(BackupsTestData.Row(
            4, integrity: uniffi.fauna_backups_machine.RowIntegrity.Implicated));
        Assert.Contains("integrity problem", implicated);
    }

    /// `last-backed-up` is ONE element off the machine's derivation (the SELECTED
    /// set's newest snapshot `created_at`), and the windows `"Last: "` prefix — a
    /// hardcoded English string in front of a value derived from the list's first
    /// row — is gone with it.
    [Fact]
    public async Task LastBackedUp_ComesFromTheMachine_AndDropsTheLastPrefix()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                selected: "documents", lastBackedUp: 1_752_000_000,
                rows: new[] { BackupsTestData.Row(7) }),
        };
        var vm = WithMachine(machine);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.DoesNotContain("Last: ", vm.LastBackedUpText);
        Assert.Contains("Last backed up", vm.LastBackedUpText);

        // A set with no snapshots renders "never" — not the previous empty string,
        // and never a carried-over value from another set.
        machine.Current = BackupsTestData.Snapshot(selected: "empty");
        vm.Apply();
        Assert.Contains("never", vm.LastBackedUpText);
    }

    /// The retention pin, after the adoption. windows used to send `["manual"]` on
    /// every manual create, and a tag is a retention SHIELD — the nest's pruner
    /// spares any tagged snapshot when the policy names no `keep_tags`, so every
    /// manual snapshot a windows user took was exempt from their own retention
    /// forever (fixed 2026-08-10 ahead of this leg).
    ///
    /// It cannot come back: the machine's create seam has NO `tags` parameter at
    /// all — a structural ruling, not a convention — so there is no argument left
    /// to get wrong. What this pins is that the page still goes through THAT seam
    /// rather than re-opening a direct `fauna.filesync.snapshot.create` call.
    [Fact]
    public async Task CreateSnapshot_GoesThroughTheMachineSeam_WhichCannotCarryATag()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(selected: "photos"),
        };
        var rpc = new MockNestRpcClient();
        var vm = WithMachine(machine, rpc);

        await vm.CreateSnapshotCommand.ExecuteAsync(null);

        Assert.Contains("CreateSnapshot", machine.Calls);
        Assert.DoesNotContain("SnapshotCreateFolder", rpc.Calls);
    }

    [Fact]
    public async Task Delete_DispatchesTheMachineGesture_WithTheRowId()
    {
        var machine = new FakeBackupsMachine();
        var vm = WithMachine(machine);

        await vm.DeleteSnapshotCommand.ExecuteAsync(42L);

        Assert.Contains("DeleteSnapshot", machine.Calls);
        Assert.Equal(42L, machine.LastDeleted);
    }

    [Fact]
    public async Task Undelete_DispatchesTheMachineGesture_WithTheRowId()
    {
        var machine = new FakeBackupsMachine();
        var vm = WithMachine(machine);

        await vm.UndeleteSnapshotCommand.ExecuteAsync(42L);

        Assert.Contains("UndeleteSnapshot", machine.Calls);
        Assert.Equal(42L, machine.LastUndeleted);
    }

    /// § *Soft-deleted rows*: `snapshot-undelete-button` renders ONLY on a
    /// `SoftDeleted` row — presence is the state observable (same shape as
    /// `snapshot-prune-execute-button`), never a count, since the nest's `list`
    /// keeps soft-deleted rows for the whole 30-day window either way.
    [Fact]
    public async Task Recoverable_IsTrueOnlyForSoftDeletedRows()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                selected: "documents",
                rows: new[]
                {
                    BackupsTestData.Row(1),
                    BackupsTestData.Row(
                        2, state: new uniffi.fauna_backups_machine.SnapshotState.SoftDeleted(1_752_000_000)),
                    BackupsTestData.Row(
                        3, state: new uniffi.fauna_backups_machine.SnapshotState.DeletionPending(1_752_000_000)),
                }),
        };
        var vm = WithMachine(machine);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.Snapshots[0].Recoverable);
        Assert.True(vm.Snapshots[1].Recoverable);
        Assert.False(vm.Snapshots[2].Recoverable);
    }

    /// Single-flight covers the undelete gesture too: while ANY op is in flight
    /// every row action is disabled, `Recoverable` included in spirit —
    /// `ActionsEnabled` is the one predicate every per-row control gates on, so a
    /// soft-deleted row's button still renders (the affordance) but is disabled.
    [Fact]
    public async Task InProgressOp_NamesUndelete_AndDisablesTheRecoverableRow()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                selected: "documents",
                rows: new[]
                {
                    BackupsTestData.Row(
                        2, state: new uniffi.fauna_backups_machine.SnapshotState.SoftDeleted(1_752_000_000)),
                },
                op: uniffi.fauna_backups_machine.BackupOp.Undelete),
        };
        var vm = WithMachine(machine);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("Recovering a snapshot…", vm.BusyText);
        Assert.True(vm.Snapshots[0].Recoverable);
        Assert.False(vm.Snapshots[0].ActionsEnabled);
    }

    /// Prune is preview-first over the set's OWN resting policy. windows used to
    /// send a client-supplied daily/weekly/monthly triple, which overrode whatever
    /// the owner had configured for the set (Architectural rule 5). The gesture now
    /// takes no policy at all, and the execute is reachable only from a preview.
    [Fact]
    public async Task Prune_PreviewsFirst_AndSuppliesNoPolicy()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(selected: "documents"),
        };
        var rpc = new MockNestRpcClient();
        var vm = WithMachine(machine, rpc);

        await vm.PruneCommand.ExecuteAsync(null);

        Assert.Contains("PrunePreview", machine.Calls);
        Assert.DoesNotContain("PruneExecute", machine.Calls);
        Assert.DoesNotContain("SnapshotPrune", rpc.Calls);
        Assert.Null(rpc.LastPrune);
    }

    /// The two no-op policy states say WHY nothing would be pruned rather than
    /// showing an empty success — and neither arms the execute, which would
    /// otherwise promise an effect it cannot have.
    [Fact]
    public async Task PrunePreview_NoPolicy_SaysSo_AndOffersNoExecute()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                selected: "documents",
                preview: BackupsTestData.Preview(uniffi.fauna_backups_machine.PolicyState.NotSet)),
        };
        var vm = WithMachine(machine);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.HasPrunePreview);
        Assert.False(vm.PruneExecutable);
        Assert.Contains(vm.PrunePreviewLines, l => l.Contains("No retention policy"));

        // An applied policy WITH candidates arms it and lists them.
        machine.Current = BackupsTestData.Snapshot(
            selected: "documents",
            preview: BackupsTestData.Preview(
                uniffi.fauna_backups_machine.PolicyState.Applied,
                wouldPrune: 2, remaining: 5,
                candidates: new[]
                {
                    new uniffi.fauna_backups_machine.PruneCandidate(9, 1_752_000_000, Array.Empty<string>()),
                }));
        vm.Apply();

        Assert.True(vm.PruneExecutable);
        Assert.Contains(vm.PrunePreviewLines, l => l.Contains("2 would be deleted"));
        Assert.Contains(vm.PrunePreviewLines, l => l.Contains("#9"));
    }

    /// Architectural rule 6: a check that COMPLETES with findings is a result, not
    /// an error. The old windows path pushed the findings into `error-message`,
    /// which is exactly what the rule forbids — an error banner claims the check
    /// failed to run.
    [Fact]
    public async Task Check_WithFindings_RendersAResult_AndLeavesErrorMessageClear()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                selected: "documents",
                check: BackupsTestData.Check(ok: false, missingChunks: 3)),
        };
        var vm = WithMachine(machine);

        await vm.CheckIntegrityCommand.ExecuteAsync(null);

        Assert.Contains("Check", machine.Calls);
        Assert.NotNull(vm.CheckResultText);
        Assert.Contains("3 missing chunks", vm.CheckResultText);
        Assert.Null(vm.ErrorMessage);
    }

    /// The verdict is the shared `is_ok` field, READ — never re-derived from the
    /// error counts (ratified 2026-06-01). A reply with is_ok = true and zero
    /// counts must render the passing sentence.
    [Fact]
    public void CheckResult_ReadsTheSharedIsOkField()
    {
        Assert.Contains("passed", BackupsViewModel.CheckResultTextFor(BackupsTestData.Check(ok: true)));
        Assert.Contains("found problems",
            BackupsViewModel.CheckResultTextFor(BackupsTestData.Check(ok: false)));
    }

    [Fact]
    public async Task SelectFolder_DispatchesTheMachineGesture()
    {
        var machine = new FakeBackupsMachine();
        var vm = WithMachine(machine);

        await vm.SelectFolderCommand.ExecuteAsync("photos");

        Assert.Equal("photos", machine.LastSelectedFolder);
    }

    /// Single-flight: while ANY op runs, every mutating control is disabled — one
    /// predicate, so no per-row control can drift off it.
    [Fact]
    public async Task InProgressOp_DisablesEveryRowAction_AndNamesTheOp()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                selected: "documents",
                rows: new[] { BackupsTestData.Row(7) },
                op: uniffi.fauna_backups_machine.BackupOp.Create),
        };
        var vm = WithMachine(machine);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.IsBusy);
        Assert.Equal("Creating a snapshot…", vm.BusyText);
        Assert.False(vm.Snapshots[0].ActionsEnabled);
    }

    /// The detail read projects the machine's file rows, and the download
    /// affordance is a REGULAR-file gesture — a directory row gets no dead button.
    [Fact]
    public async Task OpenSnapshot_ProjectsFileRows_AndOnlyRegularFilesCanDownload()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                selected: "documents",
                detail: BackupsTestData.Detail(7,
                    BackupsTestData.File("a/b.txt", size: 512),
                    BackupsTestData.File("a", fileType: "dir"))),
        };
        var vm = WithMachine(machine);

        await vm.OpenSnapshotAsync(7);

        Assert.Contains("OpenSnapshot", machine.Calls);
        Assert.Equal(7, vm.OpenSnapshotId);
        Assert.Equal(2, vm.DetailFiles.Count);
        Assert.Equal("a/b.txt", vm.DetailFiles[0].Path);
        Assert.Equal("512 B", vm.DetailFiles[0].SizeText);
        Assert.True(vm.DetailFiles[0].CanDownload);
        Assert.False(vm.DetailFiles[1].CanDownload);
    }

    /// The enable predicate is the machine's — the shared
    /// `immediate_delete_button_enabled` with its REAL in-flight flag threaded in.
    /// The fake answers independently of the typed inputs, so a VM that re-derived
    /// the four-way `&&` locally (windows was the last hand-rolled copy) would
    /// disagree with it here.
    [Fact]
    public void ImmediateDeleteEnabled_CallsTheSharedPredicate_NeverReDerivesIt()
    {
        var machine = new FakeBackupsMachine { ImmediateDeleteAnswer = true };
        var vm = WithMachine(machine);
        vm.OpenImmediateDelete(7);

        // Inputs that a hand-rolled predicate would reject (neither matches).
        vm.ImmediateDeleteConfirmInput = "nonsense";
        Assert.True(vm.ImmediateDeleteEnabled);
        Assert.Contains("ImmediateDeleteEnabled", machine.Calls);

        // …and inputs it would accept, with the machine saying no.
        machine.ImmediateDeleteAnswer = false;
        vm.ImmediateDeleteConfirmInput = "7";
        vm.ImmediateDeleteAcknowledgeInput = vm.ImmediateDeleteAckText;
        Assert.False(vm.ImmediateDeleteEnabled);
    }

    /// The friction-bar modal closes on the ROW LEAVING the machine's list, never
    /// on the call returning: a `hard_floor_breach` refusal returns from the same
    /// call and must leave the modal and the typed inputs standing for a retry
    /// (linux's lesson, inherited through the § Snapshot-list shape ledger).
    [Fact]
    public async Task ImmediateDelete_StaysOpenWhenTheRowSurvives_ClosesWhenItLeaves()
    {
        var machine = new FakeBackupsMachine
        {
            ImmediateDeleteAnswer = true,
            Current = BackupsTestData.Snapshot(
                selected: "documents", rows: new[] { BackupsTestData.Row(7) }),
        };
        var vm = WithMachine(machine);
        vm.OpenImmediateDelete(7);
        vm.ImmediateDeleteConfirmInput = "7";
        vm.ImmediateDeleteAcknowledgeInput = vm.ImmediateDeleteAckText;

        // Refused: the row is still listed.
        await vm.ConfirmImmediateDeleteAsync();
        Assert.Equal((7L, "7", vm.ImmediateDeleteAckText), machine.LastImmediateDelete);
        Assert.True(vm.ImmediateDeleteOpen);
        Assert.Equal("7", vm.ImmediateDeleteConfirmInput);

        // Landed: the row has left the list.
        machine.Current = BackupsTestData.Snapshot(selected: "documents");
        await vm.ConfirmImmediateDeleteAsync();
        Assert.False(vm.ImmediateDeleteOpen);
    }

    /// The machine localizes its own failures into `snapshot.error`; the page's
    /// `error-message` element renders exactly that, and a later success clears it.
    [Fact]
    public async Task MachineError_SurfacesOnErrorMessage_AndClearsOnTheNextSuccess()
    {
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                error: new uniffi.fauna_core.LocalizedText("errors/network", new Dictionary<string, string>())),
        };
        var vm = WithMachine(machine);

        await vm.LoadCommand.ExecuteAsync(null);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));

        machine.Current = BackupsTestData.Snapshot();
        vm.Apply();
        Assert.Null(vm.ErrorMessage);
    }

    // ── Single-file restore (snapshot-file-download-button) —
    //    backups.md § Layout & flow. The byte fetch is the shared-Rust
    //    client-side walk (download_snapshot_file_bytes over the RPC seam), so it
    //    works on sealed snapshots; the save step goes through the
    //    ISnapshotFileSaver seam (native dialog in production, directory
    //    write under e2e). The legacy per-app HTTP route must gain no callers
    //    (backups.md § Where logic lives), which the mechanism test below pins. ──

    private sealed class FakeSnapshotFileSaver : ISnapshotFileSaver
    {
        public readonly List<(string FileName, byte[] Data)> Saved = new();

        public Task<string?> SaveAsync(string suggestedFileName, byte[] data,
            CancellationToken ct = default)
        {
            Saved.Add((suggestedFileName, data));
            return Task.FromResult<string?>($"C:\\fake\\{suggestedFileName}");
        }
    }

    [Fact]
    public async Task DownloadFile_FetchesBytesOverSharedWalk_AndSavesBasename()
    {
        var bytes = new byte[] { 1, 2, 3, 4 };
        var rpc = new MockNestRpcClient { NextSnapshotFileBytes = bytes };
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                selected: "documents",
                detail: BackupsTestData.Detail(7, BackupsTestData.File("docs/a/b.txt", size: 4))),
        };
        var saver = new FakeSnapshotFileSaver();
        var vm = WithMachine(machine, rpc, deviceId: "ab12", saver: saver);
        await vm.OpenSnapshotAsync(7);

        await vm.DownloadSnapshotFileCommand.ExecuteAsync(vm.DetailFiles[0]);

        Assert.Contains("DownloadSnapshotFileBytes", rpc.Calls);
        // The walk is addressed by (snapshot id, path) — the manifest hash is
        // resolved inside the FFI and never reaches C#.
        Assert.Equal(("ab12", 7ul, "docs/a/b.txt"), rpc.LastSnapshotFileDownload);
        var saved = Assert.Single(saver.Saved);
        // The save-dialog suggestion is the file's basename, not the full
        // snapshot path (no directories in a save dialog's name field).
        Assert.Equal("b.txt", saved.FileName);
        Assert.Equal(bytes, saved.Data);
        Assert.Null(vm.ErrorMessage);
    }

    /// The mechanism guard for backups.md § Where logic lives: the byte fetch goes
    /// through the shared client-side walk over WS-RPC (no HTTP snapshot byte
    /// route exists).
    [Fact]
    public async Task DownloadFile_UsesTheSharedWalk()
    {
        var rpc = new MockNestRpcClient { NextSnapshotFileBytes = new byte[] { 9 } };
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                selected: "documents",
                detail: BackupsTestData.Detail(7, BackupsTestData.File("b.txt", size: 4))),
        };
        var saver = new FakeSnapshotFileSaver();
        var vm = WithMachine(machine, rpc, deviceId: "ab12", saver: saver);
        await vm.OpenSnapshotAsync(7);

        await vm.DownloadSnapshotFileCommand.ExecuteAsync(vm.DetailFiles[0]);

        Assert.Contains("DownloadSnapshotFileBytes", rpc.Calls);
    }

    [Fact]
    public async Task DownloadFile_SurfacesWalkError_AndSavesNothing()
    {
        var rpc = new MockNestRpcClient();
        var machine = new FakeBackupsMachine
        {
            Current = BackupsTestData.Snapshot(
                selected: "documents",
                detail: BackupsTestData.Detail(7, BackupsTestData.File("b.txt", size: 4))),
        };
        var saver = new FakeSnapshotFileSaver();
        var vm = WithMachine(machine, rpc, deviceId: "ab12", saver: saver);
        await vm.OpenSnapshotAsync(7);
        rpc.NextError = "boom";

        await vm.DownloadSnapshotFileCommand.ExecuteAsync(vm.DetailFiles[0]);

        Assert.NotNull(vm.ErrorMessage);
        Assert.Empty(saver.Saved);
    }

    // ── Backup destinations (management) — backups.md § Manage backup destinations ──

    [Fact]
    public async Task LoadDestinations_ProjectsRows_WithHostFallbackLabel()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination("id-a", "https://a.example.com/", "Aunt's nest"),
                // displayName null ⇒ label derives from the URL host (mirrors linux url_host).
                MockNestRpcClient.MakeDestination("id-b", "https://b.example.com:8443/", null),
            },
        };
        var vm = new BackupsViewModel(rpc);

        await vm.LoadDestinationsAsync();

        Assert.Contains("BackupDestinationsList", rpc.Calls);
        Assert.Equal(2, vm.Destinations.Count);
        Assert.Equal("Aunt's nest", vm.Destinations[0].Label);
        Assert.Equal("b.example.com", vm.Destinations[1].Label);
        Assert.Equal("id-b", vm.Destinations[1].Id);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task AddDestination_PersistsAndAppendsRow()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination("id-1", "https://offsite.example.com/", "Offsite"),
            },
        };
        var vm = new BackupsViewModel(rpc);

        var ok = await vm.AddDestinationAsync("https://offsite.example.com/", "Offsite");

        Assert.True(ok);
        Assert.Contains("BackupDestinationAdd", rpc.Calls);
        Assert.Equal(("https://offsite.example.com/", "Offsite"), rpc.LastDestinationAdd);
        Assert.Single(vm.Destinations);
        Assert.Equal("Offsite", vm.Destinations[0].Label);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task EditDestination_RenamesRow()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination("id-1", "https://offsite.example.com/", "Offsite-renamed"),
            },
        };
        var vm = new BackupsViewModel(rpc);

        var ok = await vm.EditDestinationAsync("id-1", "https://offsite.example.com/", "Offsite-renamed");

        Assert.True(ok);
        Assert.Equal(("id-1", "https://offsite.example.com/", "Offsite-renamed"), rpc.LastDestinationEdit);
        Assert.Equal("Offsite-renamed", vm.Destinations[0].Label);
    }

    [Fact]
    public async Task RemoveDestination_EmptiesRows()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new List<FfiBackupDestinationView>(),
        };
        var vm = new BackupsViewModel(rpc);

        var ok = await vm.RemoveDestinationAsync("id-1");

        Assert.True(ok);
        Assert.Contains("BackupDestinationRemove", rpc.Calls);
        Assert.Equal("id-1", rpc.LastDestinationRemove);
        Assert.Empty(vm.Destinations);
    }

    // ── Post-succession review pair (succession-aftermath.md § Adjudicating
    // what the aftermath carries across) — the row's `unattested` is the shared
    // at-rest verdict, projected verbatim; Keep goes through the shared door and
    // the mark clears because the RE-READ says so, never an optimistic flip.

    [Fact]
    public async Task LoadDestinations_ProjectsUnattestedVerdict()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination("id-own", "https://a.example.com/", "Own"),
                MockNestRpcClient.MakeDestination("id-carried", "https://b.example.com/", "Carried",
                    unattested: true),
            },
        };
        var vm = new BackupsViewModel(rpc);

        await vm.LoadDestinationsAsync();

        Assert.False(vm.Destinations[0].Unattested);
        Assert.True(vm.Destinations[1].Unattested);
    }

    [Fact]
    public async Task KeepDestination_CallsSharedDoorAndRendersTheReRead()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination("id-carried", "https://b.example.com/", "Carried",
                    unattested: true),
            },
        };
        var vm = new BackupsViewModel(rpc);
        await vm.LoadDestinationsAsync();
        Assert.True(vm.Destinations[0].Unattested);

        rpc.NextDestinations = new[]
        {
            MockNestRpcClient.MakeDestination("id-carried", "https://b.example.com/", "Carried"),
        };
        var ok = await vm.KeepDestinationAsync("id-carried");

        Assert.True(ok);
        Assert.Contains("BackupDestinationKeep", rpc.Calls);
        Assert.Equal("id-carried", rpc.LastDestinationKeep);
        Assert.False(vm.Destinations[0].Unattested);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task KeepDestination_FailureKeepsTheMarkAndSurfacesError()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination("id-carried", "https://b.example.com/", "Carried",
                    unattested: true),
            },
        };
        var vm = new BackupsViewModel(rpc);
        await vm.LoadDestinationsAsync();

        rpc.NextDestinationException = new InvalidOperationException("boom");
        var ok = await vm.KeepDestinationAsync("id-carried");

        Assert.False(ok);
        Assert.True(vm.Destinations[0].Unattested);
        Assert.NotNull(vm.ErrorMessage);
    }

    // ── Reclaim this device's copy ──────────────────────────────────────
    // backups.md § Manage backup destinations → *Reclaim this device's copy*.
    // The gesture DELETES the owner's only offline copy, so what these pin is
    // mostly the conservative direction: every way of not-knowing must leave the
    // button unpainted, and the ordering around a removal must never free bytes a
    // still-live custody row points at.

    /// This device's sync id, as the destination rows spell it.
    private const string ThisDevice = "aa11bb22";

    /// A client-device destination row naming <paramref name="custodian"/>.
    private static FfiBackupDestinationView ClientDeviceRow(
        string id = "dest-client", string? custodian = ThisDevice) =>
        new(@destinationId: id, @destinationNestUrl: "", @displayName: "This laptop",
            @kind: FaunaFfiMethods.DestinationKindClientDevice(),
            @custodianDeviceId: custodian, @capacityCapBytes: 1024UL * 1024UL * 1024UL);

    /// An ordinary another-nest row — never a reclaim candidate, and never
    /// offered the remove-dialog checkbox.
    private static FfiBackupDestinationView NestRow(string id = "dest-nest") =>
        new(@destinationId: id, @destinationNestUrl: "https://backup.test",
            @displayName: "Backup nest", @kind: "nest",
            @custodianDeviceId: null, @capacityCapBytes: null);

    /// A sync-agent channel answering just the two custodian calls; every other
    /// member throws through the generated fake base, so a test that reached one
    /// by accident fails loudly instead of silently returning a default.
    private sealed class FakeAgentChannel : FfiSyncAgentProvisionerFakeBase
    {
        public ulong StoreBytes { get; init; }
        public bool ReclaimStillHosting { get; init; }
        public Exception? StoreThrows { get; init; }
        public int ReclaimCalls { get; private set; }

        public override Task<FfiCustodianStoreInfo> CustodianStore()
        {
            if (StoreThrows is { } ex) throw ex;
            return Task.FromResult(FfiCustodianStoreInfoFixture.Make(bytes: StoreBytes));
        }

        public override Task<FfiCustodianReclaimOutcome> ReclaimCustodianStore()
        {
            ReclaimCalls++;
            return Task.FromResult(FfiCustodianReclaimOutcomeFixture.Make(
                stillHosting: ReclaimStillHosting, freedBytes: StoreBytes));
        }
    }

    private static BackupsViewModel WithAgent(
        MockNestRpcClient rpc, FakeAgentChannel? agent, string deviceId = ThisDevice) =>
        new(rpc, deviceId: deviceId, agentChannel: () => agent);

    /// The whole point of the row: a store this device still holds with NO
    /// destination row left to justify it. Without the row, that disk space is
    /// unrecoverable from the app.
    [Fact]
    public async Task OrphanedStoreRow_PaintsWhenAStoreOutlivesItsDestination()
    {
        var rpc = new MockNestRpcClient { NextDestinations = new List<FfiBackupDestinationView>() };
        var vm = WithAgent(rpc, new FakeAgentChannel { StoreBytes = 5UL * 1024 * 1024 });

        await vm.LoadDestinationsAsync();

        Assert.True(vm.OrphanedStoreVisible);
        Assert.NotNull(vm.OrphanedStoreText);
        // The size must be SUBSTITUTED, not left as the template's slot — a row
        // still showing "{held}" would satisfy any non-empty assertion.
        Assert.DoesNotContain("{held}", vm.OrphanedStoreText!);
    }

    /// A live custody row naming this device means the store is doing its job.
    /// Painting reclaim here would offer to delete live custody — which is
    /// exactly what re-deriving the verdict locally tends to get backwards.
    [Fact]
    public async Task OrphanedStoreRow_StaysHiddenWhileADestinationStillClaimsThisDevice()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new List<FfiBackupDestinationView> { ClientDeviceRow() },
        };
        var vm = WithAgent(rpc, new FakeAgentChannel { StoreBytes = 5UL * 1024 * 1024 });

        await vm.LoadDestinationsAsync();

        Assert.False(vm.OrphanedStoreVisible);
        Assert.Null(vm.OrphanedStoreText);
    }

    /// No bytes held ⇒ nothing to reclaim, whatever the destination list says.
    [Fact]
    public async Task OrphanedStoreRow_StaysHiddenWhenTheStoreIsEmpty()
    {
        var rpc = new MockNestRpcClient { NextDestinations = new List<FfiBackupDestinationView>() };
        var vm = WithAgent(rpc, new FakeAgentChannel { StoreBytes = 0 });

        await vm.LoadDestinationsAsync();

        Assert.False(vm.OrphanedStoreVisible);
    }

    /// The three not-knowing cases, together: no agent on this device, no device
    /// id yet, and a refusing agent. Each must answer "not orphaned" — the
    /// conservative direction for a destructive gesture.
    [Theory]
    [InlineData("no-agent")]
    [InlineData("no-device-id")]
    [InlineData("agent-throws")]
    public async Task OrphanedStoreRow_NotKnowingNeverPaintsTheButton(string how)
    {
        var rpc = new MockNestRpcClient { NextDestinations = new List<FfiBackupDestinationView>() };
        var vm = how switch
        {
            "no-agent" => WithAgent(rpc, null),
            "no-device-id" => WithAgent(rpc, new FakeAgentChannel { StoreBytes = 9999 }, deviceId: ""),
            _ => WithAgent(rpc, new FakeAgentChannel
            {
                StoreBytes = 9999,
                StoreThrows = new InvalidOperationException("agent refused"),
            }),
        };

        await vm.LoadDestinationsAsync();

        Assert.False(vm.OrphanedStoreVisible);
    }

    /// The checkbox is offered per ROW KIND, through the shared predicate — an
    /// another-nest row has no local copy to free, and an unrecognised kind must
    /// answer no rather than guess.
    [Fact]
    public async Task RemoveReclaimCheckbox_IsOfferedOnlyForClientDeviceRows()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new List<FfiBackupDestinationView>
            {
                ClientDeviceRow(), NestRow(),
                new(@destinationId: "dest-future", @destinationNestUrl: "", @displayName: "?",
                    @kind: "something-a-newer-client-wrote",
                    @custodianDeviceId: ThisDevice, @capacityCapBytes: null),
            },
        };
        var vm = WithAgent(rpc, new FakeAgentChannel());
        await vm.LoadDestinationsAsync();

        Assert.True(vm.RowIsAClientDevice("dest-client"));
        Assert.False(vm.RowIsAClientDevice("dest-nest"));
        Assert.False(vm.RowIsAClientDevice("dest-future"));
        Assert.False(vm.RowIsAClientDevice("dest-not-in-the-list"));
    }

    /// The opt-in path: removal first, reclaim second. Both must have happened.
    [Fact]
    public async Task RemoveWithReclaim_RemovesThenFreesTheStore()
    {
        var rpc = new MockNestRpcClient { NextDestinations = new List<FfiBackupDestinationView>() };
        var agent = new FakeAgentChannel { StoreBytes = 4096 };
        var vm = WithAgent(rpc, agent);

        var ok = await vm.RemoveDestinationAsync("dest-client", alsoReclaim: true);

        Assert.True(ok);
        Assert.Contains("BackupDestinationRemove", rpc.Calls);
        Assert.Equal(1, agent.ReclaimCalls);
        Assert.Null(vm.ErrorMessage);
    }

    /// Without the opt-in, removal deliberately KEEPS the local sealed store — it
    /// is the owner's only offline copy (3c-ii). The orphaned row is what then
    /// makes it reclaimable, so it must appear.
    [Fact]
    public async Task RemoveWithoutReclaim_KeepsTheStoreAndSurfacesTheOrphanedRow()
    {
        var rpc = new MockNestRpcClient { NextDestinations = new List<FfiBackupDestinationView>() };
        var agent = new FakeAgentChannel { StoreBytes = 4096 };
        var vm = WithAgent(rpc, agent);

        var ok = await vm.RemoveDestinationAsync("dest-client");

        Assert.True(ok);
        Assert.Equal(0, agent.ReclaimCalls);
        Assert.True(vm.OrphanedStoreVisible);
    }

    /// A failed reclaim must NOT read as a failed removal. The removal landed and
    /// the list no longer paints the row, so a bare reclaim error is the one
    /// reading a user cannot recover from — the message has to say both.
    [Fact]
    public async Task RemoveWithReclaim_WhenTheAgentRefuses_SaysTheRemovalStillLanded()
    {
        var rpc = new MockNestRpcClient { NextDestinations = new List<FfiBackupDestinationView>() };
        var agent = new FakeAgentChannel { StoreBytes = 4096, ReclaimStillHosting = true };
        var vm = WithAgent(rpc, agent);

        var ok = await vm.RemoveDestinationAsync("dest-client", alsoReclaim: true);

        Assert.False(ok);
        Assert.Contains("BackupDestinationRemove", rpc.Calls);
        var shown = vm.ErrorMessage ?? "";
        Assert.Contains("destination was removed", shown);
        Assert.Contains("still backing up", shown);
        Assert.DoesNotContain("{reason}", shown);
        // …and the copy that survived keeps its row, which is how the user retries.
        Assert.True(vm.OrphanedStoreVisible);
    }

    /// The standalone reclaim, off the row's own button.
    [Fact]
    public async Task ReclaimOrphanedStore_FreesTheStoreAndRetiresTheRow()
    {
        var rpc = new MockNestRpcClient { NextDestinations = new List<FfiBackupDestinationView>() };
        var agent = new FakeAgentChannel { StoreBytes = 4096 };
        var vm = WithAgent(rpc, agent);
        await vm.LoadDestinationsAsync();
        Assert.True(vm.OrphanedStoreVisible);

        // The agent reports an empty store once it has been freed.
        var freed = new FakeAgentChannel { StoreBytes = 0 };
        var vmAfter = WithAgent(rpc, freed);
        await vmAfter.LoadDestinationsAsync();

        var ok = await vm.ReclaimOrphanedStoreAsync();

        Assert.True(ok);
        Assert.Equal(1, agent.ReclaimCalls);
        Assert.Null(vm.ErrorMessage);
        Assert.False(vmAfter.OrphanedStoreVisible);
    }

    /// A still_hosting refusal is a REPORTED OUTCOME, not a silent no-op: nothing
    /// was deleted, the user must learn that, and the row stays up to retry from.
    [Fact]
    public async Task ReclaimOrphanedStore_StillHosting_ReportsAndLeavesTheRowStanding()
    {
        var rpc = new MockNestRpcClient { NextDestinations = new List<FfiBackupDestinationView>() };
        var agent = new FakeAgentChannel { StoreBytes = 4096, ReclaimStillHosting = true };
        var vm = WithAgent(rpc, agent);
        await vm.LoadDestinationsAsync();

        var ok = await vm.ReclaimOrphanedStoreAsync();

        Assert.False(ok);
        Assert.Contains("still backing up", vm.ErrorMessage ?? "");
        Assert.True(vm.OrphanedStoreVisible);
    }

    /// Convention 11: a command the app cannot honour fails LOUDLY. Reachable only
    /// if the row painted between the agent tearing down and the click.
    [Fact]
    public async Task ReclaimOrphanedStore_WithNoAgent_FailsLoudlyRatherThanSilently()
    {
        var vm = WithAgent(new MockNestRpcClient(), null);

        var ok = await vm.ReclaimOrphanedStoreAsync();

        Assert.False(ok);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.DoesNotContain("backups/", vm.ErrorMessage!);
    }

    // ── Restore after losing the nest (re-seed) ─────────────────────────
    // ui/backups.md § Restore after losing the nest. Where the button paints is
    // the shared backup_reseed_rows; the ceremony and its re-enrollment are the
    // agent's FFI face. These pin that the VM only paints what those decide.

    private static FfiReseedResult ReseedWhole() => new(
        @stopped: null,
        @resultLines: new[]
        {
            new uniffi.fauna_core.LocalizedText("backups.reseed_result_whole", new Dictionary<string, string>()),
            new uniffi.fauna_core.LocalizedText("backups.reseed_set_restored", new Dictionary<string, string>
            {
                ["set"] = "backups.reseed_set_mail",
                ["count"] = "1",
            }),
        },
        @isWhole: true,
        @reenrollError: null);

    /// A rebuilt nest's registry is empty, so the store reads orphaned — and
    /// that row carries the restore. No destination row carries it.
    [Fact]
    public async Task ReseedRows_PaintOnTheOrphanedStore()
    {
        var rpc = new MockNestRpcClient { NextDestinations = new List<FfiBackupDestinationView>() };
        var vm = WithAgent(rpc, new FakeAgentChannel { StoreBytes = 4096 });

        await vm.LoadDestinationsAsync();

        Assert.True(vm.ReseedOnOrphanedStore);
    }

    /// This device's own custodian row carries it; a nest row (its pull-back
    /// leg is not built) and another device's row do not.
    [Fact]
    public async Task ReseedRows_PaintOnThisDevicesOwnRowOnly()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new List<FfiBackupDestinationView>
            {
                ClientDeviceRow(),
                ClientDeviceRow(id: "dest-other", custodian: "cc33dd44"),
                NestRow(),
            },
        };
        var vm = WithAgent(rpc, new FakeAgentChannel { StoreBytes = 4096 });

        await vm.LoadDestinationsAsync();

        Assert.False(vm.ReseedOnOrphanedStore);
        Assert.True(vm.ReseedOnRow("dest-client"));
        Assert.False(vm.ReseedOnRow("dest-other"));
        Assert.False(vm.ReseedOnRow("dest-nest"));
    }

    /// No agent on this device ⇒ it hosts no store, so no row offers a restore.
    [Fact]
    public async Task ReseedRows_NoAgentPaintsNothing()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new List<FfiBackupDestinationView> { ClientDeviceRow() },
        };
        var vm = WithAgent(rpc, null);

        await vm.LoadDestinationsAsync();

        Assert.False(vm.ReseedOnOrphanedStore);
        Assert.False(vm.ReseedOnRow("dest-client"));
    }

    /// A whole verdict renders the shared result lines — each resolved NESTED,
    /// so the set name is a word, not a key — asks the agent with this device's
    /// id, and leaves no error.
    [Fact]
    public async Task Reseed_WholeRendersTheSharedLines()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new List<FfiBackupDestinationView>(),
            NextReseedResult = ReseedWhole(),
        };
        var vm = WithAgent(rpc, new FakeAgentChannel { StoreBytes = 4096 });
        await vm.LoadDestinationsAsync();

        var ok = await vm.ReseedAsync();

        Assert.True(ok);
        Assert.Equal(new[] { ThisDevice }, rpc.ReseedDeviceIds);
        Assert.StartsWith("Your data is back on this nest.", vm.ReseedResultText);
        Assert.Contains("Mail: 1 restored", vm.ReseedResultText);
        Assert.DoesNotContain("backups", vm.ReseedResultText!);
        Assert.Null(vm.ErrorMessage);
        Assert.False(vm.ReseedRunning);
    }

    /// A stop before any verdict lands on error-message inside
    /// backup_reseed_failed, and the result view stays absent.
    [Fact]
    public async Task Reseed_StoppedRendersOnErrorMessage()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new List<FfiBackupDestinationView>(),
            NextReseedResult = new FfiReseedResult(
                @stopped: "grant refused", @resultLines: Array.Empty<uniffi.fauna_core.LocalizedText>(),
                @isWhole: false, @reenrollError: null),
        };
        var vm = WithAgent(rpc, new FakeAgentChannel { StoreBytes = 4096 });
        await vm.LoadDestinationsAsync();

        var ok = await vm.ReseedAsync();

        Assert.False(ok);
        Assert.Null(vm.ReseedResultText);
        Assert.Contains("grant refused", vm.ErrorMessage ?? "");
        Assert.DoesNotContain("{reason}", vm.ErrorMessage!);
    }

    /// The data is back but the re-enrollment failed: the result says so AND
    /// error-message says what did not happen.
    [Fact]
    public async Task Reseed_ReenrollFailureKeepsTheResultAndSaysSo()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new List<FfiBackupDestinationView>(),
            NextReseedResult = ReseedWhole() with { @reenrollError = "list read failed" },
        };
        var vm = WithAgent(rpc, new FakeAgentChannel { StoreBytes = 4096 });
        await vm.LoadDestinationsAsync();

        await vm.ReseedAsync();

        Assert.StartsWith("Your data is back on this nest.", vm.ReseedResultText);
        Assert.Contains("list read failed", vm.ErrorMessage ?? "");
    }

    /// Convention 11: no agent ⇒ the command fails loudly, never silently.
    [Fact]
    public async Task Reseed_WithNoAgent_FailsLoudly()
    {
        var rpc = new MockNestRpcClient { NextReseedResult = ReseedWhole() };
        var vm = WithAgent(rpc, null);

        var ok = await vm.ReseedAsync();

        Assert.False(ok);
        Assert.Empty(rpc.ReseedDeviceIds);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.DoesNotContain("backups/", vm.ErrorMessage!);
    }

    /// A transport fault is a stop too: nothing was made live.
    [Fact]
    public async Task Reseed_FaultRendersAsAStop()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new List<FfiBackupDestinationView>(),
            NextReseedException = new FfiException.General("agent went away"),
        };
        var vm = WithAgent(rpc, new FakeAgentChannel { StoreBytes = 4096 });
        await vm.LoadDestinationsAsync();

        var ok = await vm.ReseedAsync();

        Assert.False(ok);
        Assert.Null(vm.ReseedResultText);
        Assert.Contains("agent went away", vm.ErrorMessage ?? "");
        Assert.False(vm.ReseedRunning);
    }

    [Fact]
    public async Task EditDestination_DifferentNest_RoutesToLocalizedError()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinationException =
                new FfiException.General("backup-destination-edit-different-nest"),
        };
        var vm = new BackupsViewModel(rpc);

        var ok = await vm.EditDestinationAsync("id-1", "https://other-nest.example.com/", "X");

        Assert.False(ok);
        Assert.Equal(Strings.Get("backups/backup_destination_edit_different_nest"), vm.ErrorMessage);
    }

    [Fact]
    public async Task AddDestination_Failure_RoutesToError()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new BackupsViewModel(rpc);

        var ok = await vm.AddDestinationAsync("https://unreachable.example.com/", "X");

        Assert.False(ok);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }

    // ── Restore surface — backups.md §§ Restore history / divergence / from destination ──

    // NOTE: the row Description / banner / detail strings are i18n-formatted via
    // Strings.Get, which returns the raw key in the unit-test host (no WinRT
    // localizer is Initialize()d — see IStringLocalizer) — the established
    // windows unit-test pattern asserts structural fields, not resolved i18n
    // text. The rendered text ("mail" / "local snapshot" / "98" / the MUA id) is
    // covered by the tier_3 e2e (test_backups_restore.py).

    [Fact]
    public async Task LoadRestore_PopulatesHistory_FlagsLocalSource()
    {
        var rpc = new MockNestRpcClient
        {
            // source_member_id None everywhere today (Plan 4 provenance pending) → local snapshot.
            NextRestoreHistory = new[] { MockNestRpcClient.MakeRestoreHistory(42, "mail", sourceMemberId: null) },
        };
        var vm = new BackupsViewModel(rpc);

        await vm.LoadRestoreAsync();

        Assert.Contains("SnapshotListRestoreHistory", rpc.Calls);
        Assert.Single(vm.RestoreHistory);
        Assert.True(vm.RestoreHistory[0].IsLocalSource);
        Assert.Equal(42, vm.RestoreHistory[0].SnapshotId);
        Assert.False(vm.RestoreHistory[0].HasDivergence);
    }

    [Fact]
    public async Task LoadRestore_NonLocalSource_ClearsLocalFlag()
    {
        var source = new byte[32];
        source[0] = 0xAB; source[1] = 0xCD; source[2] = 0xEF; source[3] = 0x01;
        var rpc = new MockNestRpcClient
        {
            NextRestoreHistory = new[] { MockNestRpcClient.MakeRestoreHistory(7, "mail", sourceMemberId: source) },
        };
        var vm = new BackupsViewModel(rpc);

        await vm.LoadRestoreAsync();

        Assert.False(vm.RestoreHistory[0].IsLocalSource);
    }

    [Fact]
    public void HexShort_TakesFirstFourBytesLowercase()
    {
        var bytes = new byte[32];
        bytes[0] = 0xAB; bytes[1] = 0xCD; bytes[2] = 0xEF; bytes[3] = 0x01;
        // Real-FFI conformance against the shared fauna_core::format::hex_short
        // (windows dotnet tests load the native dll): first 4 bytes → 8 lowercase
        // hex chars. BackupsViewModel's restore-source label consumes this.
        Assert.Equal("abcdef01", FaunaFfiMethods.HexShort(bytes));
    }

    [Fact]
    public void HexFull_RendersEveryByteLowercase()
    {
        // Real-FFI conformance against the shared fauna_core::format::hex_full
        // (windows dotnet tests load the native dll): every byte → two lowercase,
        // zero-padded hex chars (unlike HexShort, which truncates to the first 4).
        // ProfileViewModel/SubscriptionsSettingsViewModel/NestRpcClient/
        // AdminUsersViewModel consume this for actor/member-id display labels.
        Assert.Equal("", FaunaFfiMethods.HexFull(Array.Empty<byte>()));
        Assert.Equal("00", FaunaFfiMethods.HexFull(new byte[] { 0x00 }));
        Assert.Equal("0a", FaunaFfiMethods.HexFull(new byte[] { 0x0A }));
        Assert.Equal(
            "0123456789ab",
            FaunaFfiMethods.HexFull(new byte[] { 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB }));
    }

    [Theory]
    // Real-FFI conformance against the shared fauna_core::format::confidence_percent
    // (windows dotnet tests load the native dll): confidence_per_mille (0–1000) → a
    // whole percent, rounded HALF-UP. The .5% boundary cases (5→1, 995→100) are what
    // truncation (`/ 10`) would get wrong — the drift this shared contract prevents.
    // ModerationViewModel.MapAction consumes this for the moderation-queue confidence.
    [InlineData(0, 0)]
    [InlineData(4, 0)]
    [InlineData(5, 1)]
    [InlineData(920, 92)]
    [InlineData(925, 93)]
    [InlineData(994, 99)]
    [InlineData(995, 100)]
    [InlineData(1000, 100)]
    public void ConfidencePercent_RoundsHalfUp(int perMille, int expected)
    {
        Assert.Equal((uint)expected, FaunaFfiMethods.ConfidencePercent((ushort)perMille));
    }

    [Fact]
    public async Task LoadRestore_DivergenceRow_SetsBannerAndDetails()
    {
        var rpc = new MockNestRpcClient
        {
            NextRestoreHistory = new[] { MockNestRpcClient.MakeRestoreHistory(42, "mail") },
        };
        rpc.NextRestoreDivergence[42] = new[]
        {
            MockNestRpcClient.MakeRestoreDivergence(
                42, muaId: "Fauna-UI-Test/1.0", clientModseq: 99, serverModseq: 1, lostEventCount: 98),
        };
        var vm = new BackupsViewModel(rpc);

        await vm.LoadRestoreAsync();

        var row = vm.RestoreHistory[0];
        Assert.True(row.HasDivergence);
        Assert.Single(row.Divergence);   // one forensic detail row (content is e2e-verified)
    }

    [Fact]
    public async Task LoadRestore_NoDivergence_NoBanner()
    {
        var rpc = new MockNestRpcClient
        {
            // No NextRestoreDivergence entry → empty → banner suppressed (backups.md
            // "renders only when ≥1 divergence row").
            NextRestoreHistory = new[] { MockNestRpcClient.MakeRestoreHistory(42, "mail") },
        };
        var vm = new BackupsViewModel(rpc);

        await vm.LoadRestoreAsync();

        Assert.False(vm.RestoreHistory[0].HasDivergence);
        Assert.Empty(vm.RestoreHistory[0].Divergence);
    }

    [Fact]
    public async Task LoadRestore_DivergenceFetchFails_DegradesToNoBanner()
    {
        var rpc = new MockNestRpcClient
        {
            NextRestoreHistory = new[] { MockNestRpcClient.MakeRestoreHistory(42, "mail") },
            // A per-row divergence read failure must not blank the whole history.
            DivergenceError = "transient nest hiccup",
        };
        var vm = new BackupsViewModel(rpc);

        await vm.LoadRestoreAsync();

        Assert.Single(vm.RestoreHistory);
        Assert.False(vm.RestoreHistory[0].HasDivergence);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task LoadRestore_PopulatesSnapshotPicker_AndSelectsFirst()
    {
        var rpc = new MockNestRpcClient
        {
            NextMessageKindSnapshots = new[]
            {
                MockNestRpcClient.MakeSnapshotSummary(7, "mail"),
                MockNestRpcClient.MakeSnapshotSummary(8, "calendar"),
            },
        };
        var vm = new BackupsViewModel(rpc);

        await vm.LoadRestoreAsync();

        Assert.Contains("MessageKindSnapshotList", rpc.Calls);
        Assert.Equal(2, vm.RestoreSnapshots.Count);
        Assert.Equal(7, vm.RestoreSnapshots[0].Id);
        Assert.Contains("#7", vm.RestoreSnapshots[0].Label);
        Assert.Contains("mail", vm.RestoreSnapshots[0].Label);
        // The first snapshot is auto-selected so the friction bar has a target.
        Assert.Equal(7, vm.SelectedRestoreSnapshot?.Id);
    }

    [Fact]
    public async Task LoadRestore_SetsHasDestinations_WhenConfigured()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[] { MockNestRpcClient.MakeDestination("id-1", "https://b.example.com/", "B") },
        };
        var vm = new BackupsViewModel(rpc);

        await vm.LoadRestoreAsync();

        Assert.True(vm.HasDestinations);
    }

    [Fact]
    public async Task FrictionBar_EnablesOnlyWhenTypedIdMatchesSelected()
    {
        var rpc = new MockNestRpcClient
        {
            NextMessageKindSnapshots = new[] { MockNestRpcClient.MakeSnapshotSummary(7, "mail") },
        };
        var vm = new BackupsViewModel(rpc);
        await vm.LoadRestoreAsync();

        Assert.False(vm.RestoreConfirmEnabled);             // empty input
        vm.RestoreConfirmInput = "not-the-id";
        Assert.False(vm.RestoreConfirmEnabled);             // mismatch
        vm.RestoreConfirmInput = "7";
        Assert.True(vm.RestoreConfirmEnabled);              // typed id == selected snapshot id
    }

    [Fact]
    public async Task Restore_CallsRpc_WithSnapshotIdAndConfirmId_AndSetsDoneProgress()
    {
        var rpc = new MockNestRpcClient
        {
            NextMessageKindSnapshots = new[] { MockNestRpcClient.MakeSnapshotSummary(7, "mail") },
            NextRestoreReply = new uniffi.fauna_ffi.FfiSnapshotRestoreReply(7, "mail", true, ""),
        };
        var vm = new BackupsViewModel(rpc);
        await vm.LoadRestoreAsync();
        vm.RestoreConfirmInput = "7";

        await vm.RestoreAsync();

        Assert.Contains("SnapshotRestoreMessageKind", rpc.Calls);
        Assert.Equal((7L, "7"), rpc.LastRestoreCall);
        Assert.Equal(Strings.Get("backups/restore_progress_done"), vm.RestoreProgressText);
        Assert.Null(vm.RestoreWarning);
        Assert.False(vm.RestoreInProgress);
    }

    [Fact]
    public async Task Restore_ConfigAbsent_SurfacesWarning()
    {
        var rpc = new MockNestRpcClient
        {
            NextMessageKindSnapshots = new[] { MockNestRpcClient.MakeSnapshotSummary(7, "mail") },
            NextRestoreReply = new uniffi.fauna_ffi.FfiSnapshotRestoreReply(
                7, "mail", false, "restore the wrapped-MLS blobs too before restarting the bridge"),
        };
        var vm = new BackupsViewModel(rpc);
        await vm.LoadRestoreAsync();
        vm.RestoreConfirmInput = "7";

        await vm.RestoreAsync();

        Assert.Equal("restore the wrapped-MLS blobs too before restarting the bridge", vm.RestoreWarning);
    }

    // ── Immediate-delete modal (backups.md § User actions + invariant line 309) ──

    // (The enable predicate itself is pinned by
    // ImmediateDeleteEnabled_CallsTheSharedPredicate_NeverReDerivesIt above — it is
    // the machine's shared `immediate_delete_button_enabled`, not a local re-derivation.)

    [Fact]
    public void OpenImmediateDelete_TargetsSnapshot_AndStartsDisabled()
    {
        var machine = new FakeBackupsMachine { ImmediateDeleteAnswer = false };
        var vm = new BackupsViewModel(new MockNestRpcClient());
        vm.AttachMachine(machine);

        vm.OpenImmediateDelete(42);

        Assert.True(vm.ImmediateDeleteOpen);
        Assert.Equal(42, vm.ImmediateDeleteSnapshotId);
        // Both inputs cleared on open, so the confirm can never be armed by a
        // previous open's typing — never a one-click affordance.
        Assert.Equal("", vm.ImmediateDeleteConfirmInput);
        Assert.Equal("", vm.ImmediateDeleteAcknowledgeInput);
        Assert.False(vm.ImmediateDeleteEnabled);
    }

    /// A disabled confirm dispatches NOTHING and leaves the modal standing. Note
    /// the machine re-checks the friction bar on its side too, so this is the
    /// belt-and-braces half — a client that wired the button wrong still could not
    /// skip it (Architectural rule 4).
    [Fact]
    public async Task ConfirmImmediateDelete_NoopWhenDisabled()
    {
        var machine = new FakeBackupsMachine { ImmediateDeleteAnswer = false };
        var vm = new BackupsViewModel(new MockNestRpcClient());
        vm.AttachMachine(machine);
        vm.OpenImmediateDelete(42); // inputs left blank → disabled

        await vm.ConfirmImmediateDeleteAsync();

        Assert.DoesNotContain("DeleteSnapshotImmediate", machine.Calls);
        Assert.True(vm.ImmediateDeleteOpen); // stays open; no side effect
    }
}

// ── EventDetailViewModel reminder Tests ──

public class EventDetailViewModelReminderTests
{
    // 32-byte event content id (hex) the VM hands to the reminder seam.
    private const string EventIdHex =
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    [Fact]
    public async Task SetReminder_SetsOffsetAndHumanLabel_OverRpc()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventDetailViewModel(rpc) { EventId = EventIdHex };

        await vm.SetReminderCommand.ExecuteAsync("PT1H");

        Assert.Contains("CaldavSetReminder", rpc.Calls);
        Assert.Equal("PT1H", rpc.LastReminderOffset);
        Assert.True(vm.HasReminder);
        Assert.Equal("PT1H", vm.ReminderOffset);
        // Offset → human label rides the shared fauna_core::ical::reminder_label
        // (drives the event-detail-reminder-current label the e2e asserts on).
        // With no localizer loaded in the test host, Strings.Resolve falls back
        // to the raw dotted key — production resolves it to "1 hour before".
        Assert.Equal("events.reminder.hour_1", vm.ReminderCurrentLabel);
    }

    [Fact]
    public async Task RemoveReminder_ClearsOffset_OverRpc()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventDetailViewModel(rpc) { EventId = EventIdHex };
        await vm.SetReminderCommand.ExecuteAsync("PT1H");
        Assert.True(vm.HasReminder);

        await vm.RemoveReminderCommand.ExecuteAsync(null);

        // Clearing is set_reminder("") on the encrypted store (no delete kind).
        Assert.Contains("CaldavSetReminder", rpc.Calls);
        Assert.Equal("", rpc.LastReminderOffset);
        Assert.False(vm.HasReminder);
        Assert.Null(vm.ReminderOffset);
        Assert.Equal(string.Empty, vm.ReminderCurrentLabel);
    }
}

// ── Backup-destination LIVE status text (backups.md § Per-destination status read) ──
//
// Mirrors apple BackupDestinationsVMTests: the pure status → i18n text mapping
// (never / "N queued" / relative-time) + the live read over the seam + the per-row
// baseline fallback. Resolved-string assertions need a localizer (the static Strings
// singleton returns the raw key otherwise), so this shares [Collection("StringsGlobal")]
// with ValueFormatTests and installs a FakeLocalizer mirroring the generated windows
// resw (en.yaml 909-911) — keeping the named {when}/{count} placeholders intact for
// the C#-side substitution the mapper does (memory: windows resw is flat).
[Collection("StringsGlobal")]
public class BackupDestinationStatusTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["backups/backup_destination_last_upload_never"] = "Last synced: never",
            ["backups/backup_destination_last_upload"] = "Last synced: {when}",
            ["backups/backup_destination_backlog"] = "{count} queued",
            ["backups/backup_destination_last_audit_never"] = "Last checked: never",
            ["backups/backup_destination_last_audit"] = "Last checked: {when}",
            ["backups/backup_destination_last_self_audit_never"] = "Self-checked: not yet",
            ["backups/backup_destination_last_self_audit"] = "Self-checked: {when}",
            ["backups/backup_audit_alert_self_reported"] = "{destination} reports that its own copy of your data failed its check. That copy cannot be relied on.",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public BackupDestinationStatusTests() => Strings.Initialize(new FakeLocalizer());

    // A stable 32-byte device id (hex) — the same id the sync engine uses; the mock
    // seam just records it, so any non-empty hex string exercises the wiring.
    private const string DeviceIdHex =
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    // ⚠ The two audit fields default to null, which the shared projection defines as
    // "not yet audited" — never "passing" and never "failed" (fauna-ffi's
    // FfiBackupDestinationStatus::audit_state doc). That is the right default for the
    // rows below, which predate the carrier and assert nothing about auditing; a test
    // that means "audited and passing" must pass AUDIT_STATE_OK explicitly.
    private static FfiBackupDestinationStatus Status(
        string id, ulong? lastUpload, uint backlog,
        ulong? heldBytes = null, string? capState = null,
        string? auditState = null, ulong? lastAuditPassedAt = null) =>
        new FfiBackupDestinationStatus(
            id, lastUpload, backlog, heldBytes, capState, auditState, lastAuditPassedAt);

    [Fact]
    public void LastUploadText_IsNever_WhenNoStatusOrNoUpload()
    {
        // No read yet for this destination, or a destination with no upload yet — both
        // render the "never" baseline (uniform with apple/linux until an upload loop runs).
        const long now = 1_700_000_000_000L;
        Assert.Equal("Last synced: never", BackupsViewModel.LastUploadText(null, now));
        Assert.Equal("Last synced: never", BackupsViewModel.LastUploadText(Status("a", null, 3), now));
    }

    [Fact]
    public void LastUploadText_RendersTimestamp_ThroughSharedFormatter()
    {
        // A real last_upload_time (unix seconds) renders through the shared
        // ValueFormat.RelativeTime — NOT the "never" baseline. The timestamp is > 7 d
        // before `now`, so RelativeTime yields a now-stable absolute date.
        const long now = 1_700_000_000_000L;
        const ulong secs = 1_699_000_000UL;
        var mapped = BackupsViewModel.LastUploadText(Status("a", secs, 0), now);
        var when = ValueFormat.RelativeTime(now, (long)secs * 1000);
        Assert.Equal($"Last synced: {when}", mapped);
        Assert.NotEqual("Last synced: never", mapped);
    }

    [Fact]
    public void BacklogText_RendersCount_WithZeroDefault()
    {
        Assert.Equal("0 queued", BackupsViewModel.BacklogText(null));
        Assert.Equal("5 queued", BackupsViewModel.BacklogText(Status("a", null, 5)));
    }

    [Fact]
    public async Task RefreshStatuses_PopulatesLiveBacklog_OverSeam()
    {
        // LoadDestinationsAsync hydrates the rows then reads the LIVE status over the
        // seam (mirrors apple hydrate → refreshStatuses); the per-row helper renders the
        // live backlog while last-upload stays "never" until an upload loop runs.
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination("d1", "https://a.example.com/", "A"),
            },
            NextDestinationStatuses = new[] { Status("d1", null, 4) },
        };
        var vm = new BackupsViewModel(rpc, deviceId: DeviceIdHex);

        await vm.LoadDestinationsAsync();

        Assert.Contains("BackupDestinationStatus", rpc.Calls);
        Assert.Equal("4 queued", vm.BacklogText("d1"));
        Assert.Equal("Last synced: never", vm.LastUploadText("d1"));
    }

    [Fact]
    public async Task RefreshStatuses_StillReadsTheLiveProjection_WhenNoDeviceId()
    {
        // A missing device id must NOT skip the live per-destination status read
        //: the nest's fauna.backup.status projection derives the owner
        // from the authenticated connection, not from a device id, so the guard on
        // RefreshStatusesAsync gates on destination count alone — matching apple's
        // refreshStatuses (BackupDestinationsVM.swift), which never had a device-id
        // term at all. _deviceId stays real for the OTHER, genuinely device-scoped
        // calls this VM makes directly (single-file restore, client-device
        // custodian enrollment) — it just isn't a precondition of this read.
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination("d1", "https://a.example.com/", "A"),
            },
            NextDestinationStatuses = new[] { Status("d1", null, 9) },
        };
        var vm = new BackupsViewModel(rpc); // no device id

        await vm.LoadDestinationsAsync();

        Assert.Contains("BackupDestinationStatus", rpc.Calls);
        Assert.Equal("9 queued", vm.BacklogText("d1"));
    }

    [Fact]
    public void PerRowHelpers_Baseline_WhenNoStatusLoaded()
    {
        // Fresh VM, no status read done — the per-row helpers never show a blank/scary
        // value; they fall back to the "never" / "0 queued" baseline.
        var vm = new BackupsViewModel(new MockNestRpcClient());
        Assert.Equal("Last synced: never", vm.LastUploadText("d1"));
        Assert.Equal("0 queued", vm.BacklogText("d1"));
    }

    // ── Per-destination status comes from the NEST projection ───────────────
    // (backups.md § Per-destination status read.) The three tests that used to live
    // here were all about the in-app upload driver — a fake IBackupUploadDriver, the
    // ephemeral-fallback arm, and two rebuild-on-destination-change assertions. The
    // driver was DELETED 2026-08-16 (the slice-5 flip, windows arm: the source nest
    // has been the segment-backup writer since 2026-07-24), so all three assert a
    // mechanism that no longer exists. What they were collectively guarding — that
    // the page's numbers come from the nest and nothing diverts them — is now true
    // BY CONSTRUCTION: there is no second status source left to fold onto. The one
    // test below keeps the surviving, still-falsifiable half.

    /// <summary>
    /// The page renders the NEST's per-destination numbers
    /// (<c>fauna.backup.status</c>), which is the only status source after the
    /// slice-5 flip. Discriminating rather than decorative: the mock's backlog is 7,
    /// so a read that silently degraded to the not-yet-backed-up baseline would show
    /// "0 queued" and fail here.
    /// </summary>
    [Fact]
    public async Task RefreshStatuses_ReadsTheNestProjection()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination("d1", "https://a.example.com/", "A"),
            },
            NextDestinationStatuses = new[] { Status("d1", null, 7) },
        };
        var vm = new BackupsViewModel(rpc, deviceId: DeviceIdHex);

        await vm.LoadDestinationsAsync();

        Assert.Contains("BackupDestinationStatus", rpc.Calls);
        Assert.Equal("7 queued", vm.BacklogText("d1"));
    }

    // ── Client-device audit surface (backups.md § Audit-alert surface → *The
    // client-device arm*, row 136). Mirrors linux's five reference tests
    // (views/backups/destinations.rs) / apple's BackupDestinationsVMTests.swift
    // exactly — the shared predicate/formatter pins live shared-Rust side; these
    // pin only this VM's dispatch + wiring.

    /// A custodian that has never self-audited renders ABSENCE, not a verdict —
    /// reading silence as a pass would render an unverified copy as verified;
    /// reading it as a failure would raise a fleet-wide false data-loss alarm
    /// for every custodian that has not self-audited yet.
    [Fact]
    public void SelfAuditText_IsNotYet_WhenAbsent()
    {
        const long now = 1_700_000_000_000L;
        Assert.Equal("Self-checked: not yet", BackupsViewModel.SelfAuditText(null, now));
        Assert.Equal("Self-checked: not yet", BackupsViewModel.SelfAuditText(Status("a", null, 0), now));
    }

    /// The custodian's cell never borrows the owner-side wording — rendering
    /// "Last checked" over a self-report would let it wear the words of an
    /// independent verification it never received.
    [Fact]
    public void SelfAuditText_NeverWearsTheOwnerSideWording()
    {
        const long now = 1_700_000_000_000L;
        var text = BackupsViewModel.SelfAuditText(
            Status("a", null, 0, auditState: "ok", lastAuditPassedAt: 1_699_000_000UL), now);
        Assert.NotEqual("Self-checked: not yet", text);
        Assert.StartsWith("Self-checked:", text);
        Assert.DoesNotContain("Last checked", text);
    }

    /// A reported failure is loud — the only failure signal that exists for a
    /// kind the owner cannot sample.
    [Fact]
    public async Task RebuildAuditAlerts_FlagsAClientDeviceRowReportingItsOwnFailure()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination(
                    "d1", "", "This laptop", kind: FaunaFfiMethods.DestinationKindClientDevice(),
                    custodianDeviceId: "dev1"),
            },
            // Deliberately stale: the last time it PASSED. A failure never advances
            // that clock, so a flagged row must not read as fresh.
            NextDestinationStatuses = new[]
            {
                Status("d1", null, 0, auditState: "failed", lastAuditPassedAt: 1_700_000_000UL),
            },
        };
        var vm = new BackupsViewModel(rpc, deviceId: DeviceIdHex, auditStatePath: "state.json");

        await vm.LoadDestinationsAsync();

        Assert.Single(vm.AuditAlerts);
    }

    /// The audit pass is handed the sync agent's replica dir, so the covered-folder
    /// mirror plane is anchored in this device's own replica rather than the
    /// destination's list alone (backup-destinations.md § Ordinary-folder coverage →
    /// Retention + audit).
    [Fact]
    public async Task RefreshAuditAsync_HandsThePassTheSyncAgentStateDir()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[] { MockNestRpcClient.MakeDestination("d1", "", "Offsite") },
        };
        var vm = new BackupsViewModel(
            rpc, deviceId: DeviceIdHex, auditStatePath: "state.json", syncStateDir: @"C:\agent\actor");

        await vm.LoadDestinationsAsync();
        await vm.RefreshAuditAsync();

        Assert.Equal("state.json", rpc.LastAuditStatePath);
        Assert.Equal(@"C:\agent\actor", rpc.LastAuditSyncStateDir);
    }

    /// An unrecognised verdict a NEWER client wrote stays quiet — the
    /// conservative direction is the shared predicate's, not each app's.
    [Fact]
    public async Task RebuildAuditAlerts_StaysQuiet_ForAnUnrecognisedReportedVerdict()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination(
                    "d1", "", "This laptop", kind: FaunaFfiMethods.DestinationKindClientDevice(),
                    custodianDeviceId: "dev1"),
            },
            NextDestinationStatuses = new[]
            {
                Status("d1", null, 0, auditState: "degraded-in-some-newer-way", lastAuditPassedAt: null),
            },
        };
        var vm = new BackupsViewModel(rpc, deviceId: DeviceIdHex, auditStatePath: "state.json");

        await vm.LoadDestinationsAsync();

        Assert.Empty(vm.AuditAlerts);
    }

    /// A nest row is untouched by all of the above: its cell still carries this
    /// client's own independent check, dispatched by kind rather than a
    /// re-derived guess.
    [Fact]
    public async Task AuditCellText_DispatchesOnKind()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[]
            {
                MockNestRpcClient.MakeDestination("d1", "https://a.example.com/", "A"),
                MockNestRpcClient.MakeDestination(
                    "d2", "", "This laptop", kind: FaunaFfiMethods.DestinationKindClientDevice(),
                    custodianDeviceId: "dev1"),
            },
            NextDestinationStatuses = new[]
            {
                Status("d2", null, 0, auditState: "ok", lastAuditPassedAt: 1_699_000_000UL),
            },
        };
        var vm = new BackupsViewModel(rpc, deviceId: DeviceIdHex, auditStatePath: "state.json");

        await vm.LoadDestinationsAsync();

        Assert.Equal("Last checked: never", vm.AuditCellText("d1"));
        var custodianText = vm.AuditCellText("d2");
        Assert.NotEqual("Last checked: never", custodianText);
        Assert.StartsWith("Self-checked:", custodianText);
    }
}

// The spam-preferences slider↔wire conversion (ProbabilityToPerMille / PerMilleToProbability)
// is exercised directly against the shared FFI in SpamThresholdConversionFfiTests above; the
// spam *preferences* UI + save live on Settings → Privacy (SettingsViewModel /
// SettingsPrivacyPage), no longer on ModerationViewModel (this page is queue-only).

// ── Moderation queue read + train correction (ModerationViewModel) ──
//    The standalone Moderation page reads fauna.moderation.actions and renders each
//    ObligationAction row with a content-label-badge whose label/icon/accent come from
//    the shared fauna_core::content_category map (FaunaFfiMethods.ContentLabelStyle) and
//    an action label from obligation_action_label — never hard-coded per client
//    (moderation.md § Categories). The train-correction-button submits the "ham"
//    not-spam correction (matching linux/apple). These exercise the REAL native FFI
//    (FaunaFfiMethods loads the dll — reference_windows_dotnet_test_loads_native_ffi),
//    so the badge values are a cross-language conformance check of the shared map.

public class ModerationViewModelQueueTests
{
    // FfiObligationAction(id, content_type, content_id, category, confidence_per_mille, action, timestamp)
    private static FfiObligationAction Row(string category, byte action, ushort perMille, string contentId) =>
        new(0L, "post", contentId, category, perMille, action, 0L);

    [Fact]
    public async Task Load_PopulatesQueueFromModerationActionsSeam()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction>
            {
                Row("spam", action: 1, perMille: 920, contentId: "aabb"),
                Row("phishing", action: 2, perMille: 880, contentId: "ccdd"),
            },
        };
        var vm = new ModerationViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(2, vm.Actions.Count);
        Assert.Contains("ModerationActions", rpc.Calls);

        var spam = vm.Actions[0];
        Assert.Equal("aabb", spam.ContentId);
        Assert.Equal("post", spam.ContentType);
        Assert.Equal(92, spam.ConfidencePercent);          // (920 + 5) / 10, half-up
        // Badge label/icon/accent resolved through the REAL shared content_label_style.
        Assert.False(string.IsNullOrEmpty(spam.CategoryLabel));
        Assert.False(string.IsNullOrEmpty(spam.CategoryIcon));
        Assert.Equal("#DC2626", spam.CategoryAccent);      // shared spam accent (no hard-coding)
        // Action label resolved through the shared obligation_action_label map.
        Assert.False(string.IsNullOrEmpty(spam.ActionLabel));

        Assert.Equal("ccdd", vm.Actions[1].ContentId);
        Assert.Equal(88, vm.Actions[1].ConfidencePercent); // (880 + 5) / 10
    }

    [Fact]
    public async Task Load_EmptyQueue_LeavesActionsEmpty()
    {
        var rpc = new MockNestRpcClient();  // NextModerationActions defaults to empty
        var vm = new ModerationViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Empty(vm.Actions);
    }

    [Fact]
    public async Task Train_SubmitsHamVerdictForTheRow()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction>
            {
                Row("spam", action: 1, perMille: 900, contentId: "deadbeef"),
            },
        };
        var vm = new ModerationViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.TrainCommand.ExecuteAsync(vm.Actions[0]);

        Assert.NotNull(rpc.LastTrain);
        Assert.Equal("deadbeef", rpc.LastTrain!.Value.ContentId);
        Assert.Equal("ham", rpc.LastTrain!.Value.Verdict);
    }
}
