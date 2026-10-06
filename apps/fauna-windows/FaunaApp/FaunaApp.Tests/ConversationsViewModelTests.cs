using System.ComponentModel;
using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_conversations;
using uniffi.fauna_core;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the conversations compose-error surface. The
/// windows FlaUI e2e (<c>test_conversations_compose_error.py --client windows</c>)
/// flakes solo on win-arm64, so this real-FFI VM test is the authoritative gate:
/// it drives the shared <see cref="ConversationsManager"/> into the same
/// observable <c>send_state = Failed { reason }</c> a backend rejection leaves
/// (via the test-helpers <c>inject_send_failure_for_test</c> seam) and asserts the
/// VM's <see cref="ConversationsViewModel.ActiveSendErrorReason"/> read mirrors
/// linux <c>views/conversations/detail.rs</c> render() (conversations.md § Errors
/// &amp; edge cases). Joins the <c>StringsGlobal</c> collection (see
/// <see cref="ValueFormatTests"/>) because it installs a fake localizer to
/// exercise real <c>{message}</c> substitution — the test host has no
/// ResourceLoader, so <see cref="Strings.Resolve"/> falls back to the raw dotted
/// key without one.
/// </summary>
[Collection("StringsGlobal")]
public class ConversationsViewModelTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        // Mirrors the generated windows resw for conversations.unified.error_send
        // (en.yaml: 'Could not send this message: {message}').
        private static readonly Dictionary<string, string> Map = new()
        {
            ["conversations/unified/error_send"] = "Could not send this message: {message}",
            ["conversations/unified/error_add_participant"] = "Could not add them to this conversation: {message}",
            // The role-lock refusal windows started surfacing with its W5.6 (account-data-plane.md § Workstreams)
            // retirement leg (2026-08-24) — verbatim from en.yaml.
            ["conversations/errors/served_elsewhere"] =
                "Conversations are open in another instance of this app. Use them there — everything else works here.",
            // The fourth truth (conversations.md § Errors & edge cases,
            // 2026-09-13) — verbatim from en.yaml.
            ["conversations/errors/receive_stopped"] =
                "New messages stopped arriving because of an internal error. Restart the app (or reload the page) to receive them again.",
            // The fifth truth, the floor of the stack (conversations.md § Errors &
            // edge cases, 2026-09-15) — verbatim from Resources.resw. Without this
            // entry Strings.Format falls back to the raw key and a "count
            // substituted" assertion would prove nothing.
            ["conversations/errors/mail_unopenable"] =
                "{count} received messages could not be opened on this device. They were sealed to mail keys this account no longer holds, and were skipped.",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public ConversationsViewModelTests() => Strings.Initialize(new FakeLocalizer());

    /// <summary>
    /// Mirrors the production <c>ConversationsNotifyObserver</c> contract the VM
    /// depends on: <see cref="ConversationsManager"/> calls <c>OnChanged</c> after a
    /// mutation, the observer raises <see cref="INotifyPropertyChanged"/>, and the
    /// VM clears its cached snapshot in response. Without this, the VM would read a
    /// stale snapshot after <c>InjectSendFailureForTest</c> (minus the UI-thread
    /// marshalling the real observer adds, which a unit test doesn't need).
    /// </summary>
    private sealed class SyncObserver : SnapshotObserver, INotifyPropertyChanged
    {
        public event PropertyChangedEventHandler? PropertyChanged;
        public void OnChanged() =>
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
    }

    private static ConversationsManager NewManager()
    {
        var m = new ConversationsManager();
        m.InstallMockBackendsForTest();
        return m;
    }

    [Fact]
    public void ActiveSendErrorReason_is_null_when_no_send_failed()
    {
        var m = NewManager();
        var vm = new ConversationsViewModel(m, new SyncObserver());

        Assert.Null(vm.ActiveSendErrorReason);
    }

    [Fact]
    public void ActiveSendErrorReason_surfaces_the_failed_send_reason()
    {
        var m = NewManager();
        var threadId = m.CreateMlsGroup(new TypedAddress[]
        {
            new TypedAddress.Email(@emailAddress: "bob@self-nest.test"),
        });
        var vm = new ConversationsViewModel(m, new SyncObserver());

        // A freshly-created thread has not failed a send.
        Assert.Null(vm.ActiveSendErrorReason);

        const string reason = "nest rejected fauna.email.send";
        m.InjectSendFailureForTest(threadId, reason);

        // The injection stamps send_state = Failed { reason } and selects the
        // thread — the same observable state a real backend rejection leaves.
        // `reason` is a shared LocalizedText (one key + the backend detail as
        // {message}), so the VM resolves it through the windows i18n pipeline:
        // assert the detail reaches the surface AND that the raw key does not.
        // Verbatim equality could not tell a resolved template from a painted key.
        var shown = vm.ActiveSendErrorReason ?? "";
        Assert.NotEmpty(shown);
        Assert.Contains(reason, shown);
        Assert.DoesNotContain("conversations/unified", shown);
        Assert.DoesNotContain("conversations.unified", shown);
    }

    /// <summary>
    /// A failed membership/label wire op (<c>Snapshot.error</c>) must outrank a
    /// stale compose-send failure — mirrors linux's <c>page_error_text</c>
    /// precedence test (conversations.md § Errors &amp; edge cases). Both are stamped so a reversed
    /// or dropped precedence would still show non-empty text and pass an
    /// existence-only assertion — this asserts the CONTENT that resolves.
    /// </summary>
    [Fact]
    public void ActiveSendErrorReason_page_error_takes_precedence_over_a_stale_send_failure()
    {
        var m = NewManager();
        var threadId = m.CreateMlsGroup(new TypedAddress[]
        {
            new TypedAddress.Email(@emailAddress: "bob@self-nest.test"),
        });
        var vm = new ConversationsViewModel(m, new SyncObserver());

        // A stale send failure is already standing…
        const string sendReason = "nest rejected fauna.email.send";
        m.InjectSendFailureForTest(threadId, sendReason);
        Assert.Contains(sendReason, vm.ActiveSendErrorReason ?? "");

        // …then a membership op fails. The page error must win, not the send.
        const string pageReasonDetail = "cross-nest roster unreadable";
        m.InjectPageErrorForTest(new LocalizedText(
            "conversations.unified.error_add_participant",
            new() { ["message"] = pageReasonDetail }));

        var shown = vm.ActiveSendErrorReason ?? "";
        Assert.Contains(pageReasonDetail, shown);
        Assert.DoesNotContain(sendReason, shown);
        Assert.DoesNotContain("conversations/unified", shown);
        Assert.DoesNotContain("conversations.unified", shown);
    }

    /// <summary>
    /// The conversations-engine role-lock refusal outranks BOTH truths below it
    /// (<c>account-data-plane.md</c> § Multi-instance concurrency, W5.6 — windows'
    /// retirement leg, 2026-08-24). It is a STANDING condition armed at engine
    /// construction, so a page producer's unrelated failure must not mask it:
    /// were the read placed after <c>Snapshot.error</c>, the non-role-holder's
    /// page would show a transient wire-op error and hide the one condition that
    /// explains why conversations do not work in this instance at all.
    ///
    /// <para>Both reasons are stamped with distinct text, so a reversed precedence
    /// still yields non-empty output and would pass an existence-only assertion —
    /// this asserts WHICH one resolves. Mirrors apple's
    /// <c>ConversationsVM.pageError</c> and linux's <c>page_error_text</c>.</para>
    /// </summary>
    [Fact]
    public void ActiveSendErrorReason_served_elsewhere_outranks_a_standing_page_error()
    {
        var m = NewManager();
        var vm = new ConversationsViewModel(m, new SyncObserver());

        // Baseline: the role holder reports nothing — without this the assertion
        // below could pass on a VM that hardcoded the refusal.
        Assert.False(vm.EngineServedElsewhere);
        Assert.Null(vm.ActiveSendErrorReason);

        // A page error is already standing when the refusal arrives…
        const string pageReasonDetail = "cross-nest roster unreadable";
        m.InjectPageErrorForTest(new LocalizedText(
            "conversations.unified.error_add_participant",
            new() { ["message"] = pageReasonDetail }));
        Assert.Contains(pageReasonDetail, vm.ActiveSendErrorReason ?? "");

        // …the engine-construction seam arms the refusal (what
        // `conversations_session_over_manager` does on a `ServedElsewhere` init).
        m.SetEngineServedElsewhere(true);

        Assert.True(vm.EngineServedElsewhere);
        var shown = vm.ActiveSendErrorReason ?? "";
        Assert.Contains("another instance of this app", shown);
        Assert.DoesNotContain(pageReasonDetail, shown);
        Assert.DoesNotContain("conversations/errors", shown);

        // Clearing it (a later successful engine init) hands the page back to the
        // truths below — the refusal is a condition, not a latch.
        m.SetEngineServedElsewhere(false);
        Assert.Contains(pageReasonDetail, vm.ActiveSendErrorReason ?? "");
    }

    /// <summary>
    /// The fourth truth (<c>conversations.md</c> § Errors &amp; edge cases,
    /// 2026-09-13): a dead receive rail is standing like the served-elsewhere
    /// refusal, so it must outrank the snapshot error / send-state truths below
    /// it but never mask the role-lock refusal above it. Mirrors apple's
    /// <c>pageErrorRanksReceiveStoppedAboveTheSnapshotErrorAndBelowServedElsewhere</c>
    /// (<c>ConversationsPageErrorTests.swift</c>).
    /// </summary>
    [Fact]
    public void ActiveSendErrorReason_receive_stopped_outranks_a_standing_page_error_but_not_served_elsewhere()
    {
        var m = NewManager();
        var vm = new ConversationsViewModel(m, new SyncObserver());

        const string pageReasonDetail = "cross-nest roster unreadable";
        m.InjectPageErrorForTest(new LocalizedText(
            "conversations.unified.error_add_participant",
            new() { ["message"] = pageReasonDetail }));
        Assert.Contains(pageReasonDetail, vm.ActiveSendErrorReason ?? "");

        m.MarkReceiveStoppedForTest();

        Assert.True(vm.ReceiveStopped);
        var shown = vm.ActiveSendErrorReason ?? "";
        Assert.Contains("Restart the app", shown);
        Assert.DoesNotContain(pageReasonDetail, shown);
        Assert.DoesNotContain("conversations/errors", shown);

        // The role-lock refusal still outranks the dead rail.
        m.SetEngineServedElsewhere(true);
        Assert.Contains("another instance of this app", vm.ActiveSendErrorReason ?? "");
        m.SetEngineServedElsewhere(false);
    }

    /// <summary>
    /// "No gesture clears it" (<c>conversations.md</c> § Errors &amp; edge cases):
    /// only a newer receive loop over the same manager retires the notice. A
    /// producer succeeding and clearing the UNRELATED <c>Snapshot.error</c> truth
    /// must leave the dead-rail notice standing. Mirrors apple's
    /// <c>pageErrorReceiveStoppedIsNotClearedByASuccess</c>.
    /// </summary>
    [Fact]
    public void ActiveSendErrorReason_receive_stopped_is_not_cleared_by_an_unrelated_success()
    {
        var m = NewManager();
        var vm = new ConversationsViewModel(m, new SyncObserver());

        m.MarkReceiveStoppedForTest();
        Assert.Contains("Restart the app", vm.ActiveSendErrorReason ?? "");

        m.ClearPageError();

        Assert.True(vm.ReceiveStopped);
        Assert.Contains("Restart the app", vm.ActiveSendErrorReason ?? "");
    }

    // ── The fifth truth: mail the receive path skipped (the floor) ────────────
    //
    // `conversations.md` § Errors & edge cases → "A fifth truth, the floor of the
    // stack" (2026-09-15): a record the receive path cannot open is skipped, the
    // manager counts it (`unopenable_mail_count`), and the page tells the user —
    // ranked BELOW every other truth. `note_unopenable_mail` is crate-internal
    // (`MailFeed` has no uniffi derive), so these tests raise the count through
    // the Inbox-scoped `NoteUnopenableMailForTest` seam; the per-uid ledger is
    // idempotent, so two distinct uids are a count of 2.

    /// <summary>The count-free tail of the notice, verbatim from the resw.</summary>
    private const string UnopenableNoticeTail = "received messages could not be opened on this device";

    /// <summary>
    /// The notice shows when nothing else is standing, with the COUNT substituted
    /// through the generated formatter — neither the raw key nor a literal
    /// <c>{count}</c> placeholder. Two distinct uids are noted so a VM that hard-
    /// coded a 1 (or painted the notice without reading the count) cannot pass.
    /// Mirrors apple's <c>pageErrorSurfacesUnopenableMailWhenNothingElseIsStanding</c>
    /// (<c>ConversationsPageErrorTests.swift</c>).
    /// </summary>
    [Fact]
    public void ActiveSendErrorReason_shows_the_unopenable_mail_notice_with_the_count_when_nothing_else_stands()
    {
        var m = NewManager();
        var vm = new ConversationsViewModel(m, new SyncObserver());

        // Baseline: nothing skipped, nothing shown — without this the assertions
        // below could pass on a VM that painted the notice unconditionally.
        Assert.Equal(0u, vm.UnopenableMailCount);
        Assert.Null(vm.ActiveSendErrorReason);

        m.NoteUnopenableMailForTest(7);
        m.NoteUnopenableMailForTest(9);

        Assert.Equal(2u, vm.UnopenableMailCount);
        var shown = vm.ActiveSendErrorReason ?? "";
        Assert.StartsWith("2 " + UnopenableNoticeTail, shown);
        Assert.DoesNotContain("{count}", shown);
        Assert.DoesNotContain("conversations/errors", shown);
    }

    /// <summary>
    /// A failed compose-send outranks the notice — the floor is never a mask over a
    /// fresh failure of the user's own gesture. The notice is asserted standing
    /// FIRST so a VM that ranked it above the send truth (or never surfaced it)
    /// cannot pass on the send text alone.
    /// </summary>
    [Fact]
    public void ActiveSendErrorReason_a_failed_send_outranks_the_unopenable_mail_notice()
    {
        var m = NewManager();
        var threadId = m.CreateMlsGroup(new TypedAddress[]
        {
            new TypedAddress.Email(@emailAddress: "bob@self-nest.test"),
        });
        var vm = new ConversationsViewModel(m, new SyncObserver());

        m.NoteUnopenableMailForTest(7);
        Assert.Contains(UnopenableNoticeTail, vm.ActiveSendErrorReason ?? "");

        const string sendReason = "nest rejected fauna.email.send";
        m.InjectSendFailureForTest(threadId, sendReason);

        var shown = vm.ActiveSendErrorReason ?? "";
        Assert.Contains(sendReason, shown);
        Assert.DoesNotContain(UnopenableNoticeTail, shown);
    }

    /// <summary>
    /// A standing snapshot error (a failed membership/label wire op) outranks the
    /// notice, and the notice is back the moment the higher truth clears — the
    /// floor is a condition beneath the stack, never displaced for good.
    /// </summary>
    [Fact]
    public void ActiveSendErrorReason_unopenable_mail_is_the_floor_beneath_a_standing_page_error()
    {
        var m = NewManager();
        var vm = new ConversationsViewModel(m, new SyncObserver());

        m.NoteUnopenableMailForTest(7);
        Assert.Contains(UnopenableNoticeTail, vm.ActiveSendErrorReason ?? "");

        const string pageReasonDetail = "cross-nest roster unreadable";
        m.InjectPageErrorForTest(new LocalizedText(
            "conversations.unified.error_add_participant",
            new() { ["message"] = pageReasonDetail }));

        var shown = vm.ActiveSendErrorReason ?? "";
        Assert.Contains(pageReasonDetail, shown);
        Assert.DoesNotContain(UnopenableNoticeTail, shown);

        m.ClearPageError();
        Assert.Contains(UnopenableNoticeTail, vm.ActiveSendErrorReason ?? "");
    }

    /// <summary>A dead receive rail (the fourth truth) outranks the notice.</summary>
    [Fact]
    public void ActiveSendErrorReason_receive_stopped_outranks_the_unopenable_mail_notice()
    {
        var m = NewManager();
        var vm = new ConversationsViewModel(m, new SyncObserver());

        m.NoteUnopenableMailForTest(7);
        Assert.Contains(UnopenableNoticeTail, vm.ActiveSendErrorReason ?? "");

        m.MarkReceiveStoppedForTest();

        var shown = vm.ActiveSendErrorReason ?? "";
        Assert.Contains("Restart the app", shown);
        Assert.DoesNotContain(UnopenableNoticeTail, shown);
    }

    /// <summary>
    /// The role-lock refusal outranks the notice, and — a condition, not a latch —
    /// hands the page back to it when the refusal clears.
    /// </summary>
    [Fact]
    public void ActiveSendErrorReason_served_elsewhere_outranks_the_unopenable_mail_notice()
    {
        var m = NewManager();
        var vm = new ConversationsViewModel(m, new SyncObserver());

        m.NoteUnopenableMailForTest(7);
        Assert.Contains(UnopenableNoticeTail, vm.ActiveSendErrorReason ?? "");

        m.SetEngineServedElsewhere(true);

        var shown = vm.ActiveSendErrorReason ?? "";
        Assert.Contains("another instance of this app", shown);
        Assert.DoesNotContain(UnopenableNoticeTail, shown);

        m.SetEngineServedElsewhere(false);
        Assert.Contains(UnopenableNoticeTail, vm.ActiveSendErrorReason ?? "");
    }

    /// <summary>
    /// "No gesture clears it" (<c>conversations.md</c> § Errors &amp; edge cases):
    /// only the records opening after all retire an entry, so an unrelated
    /// producer succeeding (which clears <c>Snapshot.error</c>) must leave the
    /// count and the notice standing. Mirrors
    /// <see cref="ActiveSendErrorReason_receive_stopped_is_not_cleared_by_an_unrelated_success"/>.
    /// </summary>
    [Fact]
    public void ActiveSendErrorReason_unopenable_mail_notice_is_not_cleared_by_an_unrelated_success()
    {
        var m = NewManager();
        var vm = new ConversationsViewModel(m, new SyncObserver());

        m.NoteUnopenableMailForTest(7);
        Assert.Contains(UnopenableNoticeTail, vm.ActiveSendErrorReason ?? "");

        m.ClearPageError();

        Assert.Equal(1u, vm.UnopenableMailCount);
        Assert.Contains(UnopenableNoticeTail, vm.ActiveSendErrorReason ?? "");
    }

    /// <summary>
    /// A refused send must come back as a value, never as an exception. The shared
    /// manager both stamps the refusal on the snapshot AND returns it
    /// (<c>ConversationsManager::send</c>'s contract), and the page reaches this from
    /// an <c>async void</c> click handler — so a rethrow lands on the XAML dispatcher
    /// as an unhandled exception. Measured: an over-the-limit send left the app
    /// unable to run any later navigation, so the next sign-in in the same process
    /// never reached its main page. Apple's <c>ConversationsVM.send</c> swallows it
    /// the same way. "No such thread" is the one refusal reachable here without a
    /// live backend, and it throws the same generated <c>BackendException</c> a
    /// nest refusal does.
    /// </summary>
    [Fact]
    public async Task Send_returns_false_instead_of_throwing_when_the_manager_refuses()
    {
        var m = NewManager();
        var vm = new ConversationsViewModel(m, new SyncObserver());

        var sent = await vm.Send("no-such-thread");

        Assert.False(sent);
    }
}
