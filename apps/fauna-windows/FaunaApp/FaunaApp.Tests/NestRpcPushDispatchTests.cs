using FaunaApp.Core.Services;
using FaunaApp.Tests;
using Xunit;
using uniffi.fauna_ffi;

// The push-pump routing table (NestRpcClient.DispatchPush): which inbound push kinds
// windows actually reacts to, and how.
//
// This file exists because the `SyncChanged` arm was MISSING and nothing caught it.
// The routing table lived inside `RunPushPumpAsync`'s un-fakeable socket loop, so no
// unit test could see it; the push simply fell out of the switch, and the symptom —
// a peer's save taking the agent's 300 s rescan interval instead of seconds — read as
// "windows sync is slow", not as a dropped event. That is convention 11's dropped
// command one layer up: honour it or fail loudly, never fall through a `switch`.
//
// So the rule these tests encode: every FfiPushEvent variant gets an arm here that
// states what windows does with it, INCLUDING the deliberate no-ops.
//
// Adopted the shared `StaleSurfacesForPushEvent` classifier 2026-09-06
// (transport.md § Which surfaces a push invalidates): the UI-raise half of the
// CalendarChanged/SyncChanged arms is now gated on the classifier's booleans
// rather than firing unconditionally, so a regression there silently
// swallows a raise instead of mis-parsing a payload — these tests are what
// catches that.
public class NestRpcPushDispatchTests
{
    private static (NestRpcClient Rpc, FakeAgentSyncNudge Agent) Subject()
    {
        var agent = new FakeAgentSyncNudge();
        // No connection is made: DispatchPush is pure routing over an already-decoded
        // event, which is the whole point of splitting it out of the socket loop.
        var rpc = new NestRpcClient("https://nest.example", new MockCryptoService(), agent);
        return (rpc, agent);
    }

    private static FfiPushEvent.Notification AnyNotification() =>
        new(new FfiNotification(
            notificationId: 1, notifType: "mention", source: "test", senderId: null,
            contentId: null, summary: "hi", timestamp: 0));

    [Fact]
    public async Task SyncChanged_NudgesTheLocalAgentForThatSet()
    {
        var (rpc, agent) = Subject();

        await rpc.DispatchPush(new FfiPushEvent.SyncChanged("docs", null));

        Assert.Equal(new[] { "docs" }, agent.PulledNow);
    }

    [Fact]
    public async Task SyncChanged_NudgesPerSet_NotBlanket()
    {
        var (rpc, agent) = Subject();

        await rpc.DispatchPush(new FfiPushEvent.SyncChanged("docs", null));
        await rpc.DispatchPush(new FfiPushEvent.SyncChanged("photos", null));

        // The IPC verb is per-set (the agent routes to that set's wake channel), so
        // the set name must be threaded through rather than collapsed into a
        // "something changed" blanket pull.
        Assert.Equal(new[] { "docs", "photos" }, agent.PulledNow);
    }

    [Fact]
    public async Task AnAgentThatRefusesTheNudgeIsNotAnError()
    {
        var (rpc, agent) = Subject();

        // The agent is absent on a device where sync was never set up, and a set with
        // no resident engine drops the nudge silently. Both are ordinary states: the
        // rescan tick is the correctness backstop, so the dispatch must complete
        // normally rather than throw and kill the pump for every OTHER push kind.
        await rpc.DispatchPush(new FfiPushEvent.SyncChanged("never-bound", null));

        Assert.Equal(new[] { "never-bound" }, agent.PulledNow);
    }

    [Fact]
    public async Task CalendarChanged_RaisesItsEvent_AndNeverNudgesTheSyncAgent()
    {
        var (rpc, agent) = Subject();
        (string Actor, string Calendar)? seen = null;
        rpc.CalendarPushChanged += (a, c) => seen = (a, c);

        await rpc.DispatchPush(new FfiPushEvent.CalendarChanged("actor1", "cal1"));

        Assert.Equal(("actor1", "cal1"), seen);
        Assert.Empty(agent.PulledNow);
    }

    [Fact]
    public async Task AddressBookChanged_RaisesItsEvent_AndNeverNudgesTheSyncAgent()
    {
        // transport.md § Push events (`fauna.addressbook.changed`): the carddav twin of
        // the calendar arm. The raise is gated on `StaleSurfacesForPushEvent(ev).addressBook`
        // (the shared classifier), and rides its own event — distinct from the
        // `FfiPushEvent` variant name, like `CalendarPushChanged` — so the Contacts page
        // subscribes to one seam event rather than matching the FFI type.
        var (rpc, agent) = Subject();
        (string Actor, string Book)? seen = null;
        rpc.AddressBookPushChanged += (a, b) => seen = (a, b);

        await rpc.DispatchPush(new FfiPushEvent.AddressBookChanged("actor1", "book1"));

        Assert.Equal(("actor1", "book1"), seen);
        Assert.Empty(agent.PulledNow);
    }

    [Fact]
    public async Task AddressBookChanged_DoesNotRaiseTheCalendarEvent()
    {
        // The two carddav/caldav twins must not cross-fire: a card write re-reads the
        // Address Book, never the Events page.
        var (rpc, _) = Subject();
        int calendarRaises = 0;
        rpc.CalendarPushChanged += (_, _) => calendarRaises++;

        await rpc.DispatchPush(new FfiPushEvent.AddressBookChanged("actor1", "book1"));

        Assert.Equal(0, calendarRaises);
    }

    [Theory]
    [InlineData("fauna.bridges.push.spam_model_updated")]
    [InlineData("fauna.bridges.push.spam_model_reset")]
    public async Task SpamModelPushes_RaiseMailSpamPushChanged_AndNeverNudgeTheSyncAgent(string kind)
    {
        // mail-spam.md § Implementation status today, item 5: an undo or reset on another
        // device arrives as `FfiPushEvent.Other`, and only the shared classifier's
        // `mailSpam` flag decides whether the open Spam panel re-hydrates.
        var (rpc, agent) = Subject();
        int raised = 0;
        rpc.MailSpamPushChanged += () => raised++;

        await rpc.DispatchPush(new FfiPushEvent.Other(kind));

        Assert.Equal(1, raised);
        Assert.Empty(agent.PulledNow);
    }

    [Theory]
    [InlineData("fauna.bridges.push.import_progress")]
    [InlineData("fauna.bridges.push.import_error")]
    [InlineData("fauna.bridges.push.import_complete")]
    [InlineData("fauna.bridges.push.import_session_changed")]
    public async Task ImportPushes_RaiseMailImportPushChanged_AndNothingElse(string kind)
    {
        // mailbox-migration.md § A failed import is a record the wizard shows until the
        // user moves on: the three import events arrive as `FfiPushEvent.Other`, and only
        // the shared classifier's `mailImport` flag decides whether the open Import
        // wizard re-reads. They must not cross-fire the spam twin or nudge the agent.
        var (rpc, agent) = Subject();
        int raised = 0, spamRaised = 0;
        rpc.MailImportPushChanged += () => raised++;
        rpc.MailSpamPushChanged += () => spamRaised++;

        await rpc.DispatchPush(new FfiPushEvent.Other(kind));

        Assert.Equal(1, raised);
        Assert.Equal(0, spamRaised);
        Assert.Empty(agent.PulledNow);
    }

    [Theory]
    [InlineData("fauna.bridges.push.export_progress")]
    [InlineData("fauna.bridges.push.export_error")]
    [InlineData("fauna.bridges.push.export_complete")]
    [InlineData("fauna.bridges.push.export_session_changed")]
    public async Task ExportPushes_RaiseMailExportPushChanged_AndNothingElse(string kind)
    {
        // mail-export.md § A failed export is a record the wizard shows until the user
        // moves on: the three export events arrive as `FfiPushEvent.Other`, and only the
        // shared classifier's `mailExport` flag decides whether the open Export wizard
        // re-reads. They must not cross-fire the import or spam twins or nudge the agent.
        var (rpc, agent) = Subject();
        int raised = 0, importRaised = 0, spamRaised = 0;
        rpc.MailExportPushChanged += () => raised++;
        rpc.MailImportPushChanged += () => importRaised++;
        rpc.MailSpamPushChanged += () => spamRaised++;

        await rpc.DispatchPush(new FfiPushEvent.Other(kind));

        Assert.Equal(1, raised);
        Assert.Equal(0, importRaised);
        Assert.Equal(0, spamRaised);
        Assert.Empty(agent.PulledNow);
    }

    [Fact]
    public async Task ImportPushes_DoNotRaiseTheExportTwin()
    {
        var (rpc, _) = Subject();
        int exportRaised = 0;
        rpc.MailExportPushChanged += () => exportRaised++;

        await rpc.DispatchPush(new FfiPushEvent.Other("fauna.bridges.push.import_error"));

        Assert.Equal(0, exportRaised);
    }

    [Fact]
    public async Task SyncChanged_AlsoRaisesFolderChangedPushed()
    {
        // Pins the classifier adoption (transport.md § Which surfaces a push
        // invalidates): the UI raise below `DispatchPush`'s SyncChanged arm is
        // now gated on `StaleSurfacesForPushEvent(ev).media` rather than firing
        // unconditionally — `fauna.sync.changed` maps to `media: true`, so this
        // must still fire. A regression here would mean the classifier gate
        // silently swallowed the raise for a kind it is supposed to allow.
        var (rpc, agent) = Subject();
        string? seenFolder = null;
        rpc.FolderChangedPushed += (f, _) => seenFolder = f;

        await rpc.DispatchPush(new FfiPushEvent.SyncChanged("docs", null));

        Assert.Equal("docs", seenFolder);
        // The agent nudge is a side effect, not a staleness reaction — stays
        // unconditional regardless of the classifier gate on the UI raise.
        Assert.Equal(new[] { "docs" }, agent.PulledNow);
    }

    [Fact]
    public async Task SealedSyncChanged_RaisesTheBlankFolderAndHashPair()
    {
        // A sealed set's nudge carries a blank `folder` and only `folderHash`
        // (path-sealing.md § the set-name plane): the UI raise must hand both
        // to the subscriber, whose gate matches through SyncChangedGate.
        var (rpc, _) = Subject();
        string? seenFolder = null;
        byte[]? seenHash = null;
        rpc.FolderChangedPushed += (f, h) => { seenFolder = f; seenHash = h; };
        var hash = uniffi.fauna_ffi.FaunaFfiMethods.WebdavSetNameHash("docs");

        await rpc.DispatchPush(new FfiPushEvent.SyncChanged("", hash));

        Assert.Equal("", seenFolder);
        Assert.Equal(hash, seenHash);
    }

    [Fact]
    public void SyncChangedGate_MatchesByHash_WhenFolderIsBlank()
    {
        var hash = uniffi.fauna_ffi.FaunaFfiMethods.WebdavSetNameHash("docs");

        Assert.True(SyncChangedGate.NamesSet("", hash, "docs"));
        Assert.False(SyncChangedGate.NamesSet("", hash, "photos"));
    }

    [Fact]
    public void SyncChangedGate_FallsBackToTheName_WithoutAHash()
    {
        Assert.True(SyncChangedGate.NamesSet("docs", null, "docs"));
        Assert.False(SyncChangedGate.NamesSet("docs", null, "photos"));
        Assert.False(SyncChangedGate.NamesSet("", null, "docs"));
    }

    [Fact]
    public async Task NotificationAndResync_ReHydrate_AndNeverNudgeTheSyncAgent()
    {
        var (rpc, agent) = Subject();
        int reconnects = 0;
        rpc.Reconnected += () => reconnects++;

        await rpc.DispatchPush(AnyNotification());
        await rpc.DispatchPush(new FfiPushEvent.ResyncRequired(3));

        Assert.Equal(2, reconnects);
        Assert.Empty(agent.PulledNow);
    }

    [Fact]
    public async Task OnlyResyncRequired_AndAReconnect_MarkTheDevicesSurfaceStale()
    {
        var (rpc, _) = Subject();
        int stale = 0;
        rpc.DevicesStale += () => stale++;

        // A Notification fires Reconnected too, but says nothing about the folder rows.
        await rpc.DispatchPush(AnyNotification());
        Assert.Equal(0, stale);

        await rpc.DispatchPush(new FfiPushEvent.ResyncRequired(3));
        Assert.Equal(1, stale);

        // The reconnect pump's per-reconnect step (transport.md: `devices` is
        // reconnect-and-resync only).
        rpc.OnReconnectObserved();
        Assert.Equal(2, stale);
    }

    [Fact]
    public async Task AccountUpdatedAndOther_AreDELIBERATENoOps()
    {
        var (rpc, agent) = Subject();
        int reconnects = 0;
        rpc.Reconnected += () => reconnects++;

        // Pinned so that "windows ignores this" stays a decision on the record. If a
        // future session wires a surface to either kind, this test is what tells them
        // the silence here was chosen rather than overlooked.
        await rpc.DispatchPush(new FfiPushEvent.AccountUpdated(new[] { "handle" }, 0));
        await rpc.DispatchPush(new FfiPushEvent.Other("fauna.bridge.something"));

        Assert.Equal(0, reconnects);
        Assert.Empty(agent.PulledNow);
    }
}
