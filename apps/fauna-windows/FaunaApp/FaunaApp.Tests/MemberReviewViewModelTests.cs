using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_conversations;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The permanent Settings → Members To Review sub-page VM
/// (docs/goal/behavior/succession-aftermath.md § Propagation → *Removing a
/// flagged member*). Mirrors the invariants
/// tui's own <c>member_review.rs</c> test suite pins:
/// <list type="number">
/// <item><b>No sweep gate</b> — the roster renders regardless of any ceremony
/// state; <see cref="MemberReviewViewModel.LoadAsync"/> takes only the roster
/// and a manager, nothing else.</item>
/// <item><b>An unnamed person still gets a closable row</b> — a <c>null</c>
/// manager (or one that cannot resolve the handle) renders under the
/// "no longer in any of your groups" wording rather than dropping the row.
/// </item>
/// <item><b>The verdict is DERIVED, never chosen</b> — <see cref="MemberReviewViewModel.RemoveAsync"/>
/// never writes anything itself; it reads what the mock's
/// <see cref="MockNestRpcClient.NextMemberReviewRemoveResult"/> "earned" and
/// re-reads only when the eviction was complete.</item>
/// </list>
///
/// <para>[Collection("StringsGlobal")] because row composition resolves
/// shared <c>LocalizedText</c> through <see cref="Strings"/>, whose localizer
/// is process-global (same reason <c>NostrViewModelTests</c> needs it). This
/// class additionally installs its own <see cref="FakeLocalizer"/> in its
/// constructor (runs before every test) mapping the real
/// <c>settings/recovery_kit/review_*</c> templates: unlike a plain
/// self-consistent round-trip through <see cref="Strings.Get"/>, the
/// <c>review_row</c> template's <c>{who}</c>/<c>{reason}</c> substitution
/// only actually happens with a real template string in place — a raw-key
/// echo contains neither placeholder, so the composition would silently
/// discard both without one.</para>
/// </summary>
[Collection("StringsGlobal")]
public class MemberReviewViewModelTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["settings/recovery_kit/review_row"] = "{who} — {reason}",
            ["settings/recovery_kit/review_unknown_person"] = "Someone no longer in any of your groups",
            ["settings/recovery_kit/review_reason_compromise"] = "was in your groups before you recovered your account",
            ["settings/recovery_kit/review_reason_other"] = "this could not confirm their identity",
            ["settings/recovery_kit/review_keep"] = "Keep",
            ["settings/recovery_kit/review_remove"] = "Remove From My Groups",
            ["settings/recovery_kit/review_remove_partial"] =
                "Removed {who} from {removed} of {groups} of your group conversations.",
            ["settings/recovery_kit/review_remove_done_here"] =
                "Removed {who} from {removed} of your group conversations.",
            ["settings/recovery_kit/review_remove_none_here"] =
                "{who} is not in any of your group conversations on this device.",
            ["settings/recovery_kit/review_remove_folder_seats"] =
                "They are also in {seats} shared folder(s).",
            ["settings/recovery_kit/review_remove_unsynced_seats"] =
                "They are also in {seats} group chat(s) not yet synced to this device.",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public MemberReviewViewModelTests() => Strings.Initialize(new FakeLocalizer());

    private static ConversationsManager NewManager()
    {
        var m = new ConversationsManager();
        m.InstallMockBackendsForTest();
        return m;
    }

    [Fact]
    public async Task LoadAsync_EmptyRoster_NoRows()
    {
        var rpc = new MockNestRpcClient { NextMemberReviews = System.Array.Empty<FfiMemberReview>() };
        var vm = new MemberReviewViewModel(rpc);

        await vm.LoadAsync(null);

        Assert.Empty(vm.Reviews);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task LoadAsync_ProjectsOneRowPerPerson()
    {
        var rpc = new MockNestRpcClient
        {
            NextMemberReviews = new[]
            {
                MockNestRpcClient.MakeMemberReview(person: new byte[32]),
                MockNestRpcClient.MakeMemberReview(person: Enumerable.Repeat((byte)2, 32).ToArray()),
            },
        };
        var vm = new MemberReviewViewModel(rpc);

        await vm.LoadAsync(null);

        Assert.Equal(2, vm.Reviews.Count);
        Assert.All(vm.Reviews, r => Assert.False(string.IsNullOrEmpty(r.DisplayText)));
    }

    [Fact]
    public async Task LoadAsync_ANoManagerPersonStillGetsAClosableRow()
    {
        // An item nobody can name is an item nobody can close — the row must
        // still render, with the "no longer in any of your groups" wording.
        var rpc = new MockNestRpcClient
        {
            NextMemberReviews = new[] { MockNestRpcClient.MakeMemberReview() },
        };
        var vm = new MemberReviewViewModel(rpc);

        await vm.LoadAsync(null);

        var row = Assert.Single(vm.Reviews);
        Assert.Contains(Strings.Get("settings/recovery_kit/review_unknown_person"), row.DisplayText);
    }

    [Fact]
    public async Task LoadAsync_AnUnknownReasonIsRenderedNotDropped()
    {
        // An unrecognised reason string (a newer build's) round-trips
        // verbatim rather than collapsing to a known-reason wording.
        var rpc = new MockNestRpcClient
        {
            NextMemberReviews = new[]
            {
                MockNestRpcClient.MakeMemberReview(reasons: new[] { "something-a-newer-build-invented" }),
            },
        };
        var vm = new MemberReviewViewModel(rpc);

        await vm.LoadAsync(null);

        var row = Assert.Single(vm.Reviews);
        Assert.Contains(Strings.Get("settings/recovery_kit/review_reason_other"), row.DisplayText);
    }

    [Fact]
    public async Task KeepAsync_RecordsThenRereads()
    {
        var rpc = new MockNestRpcClient
        {
            NextMemberReviews = new[] { MockNestRpcClient.MakeMemberReview() },
        };
        var vm = new MemberReviewViewModel(rpc);
        await vm.LoadAsync(null);
        Assert.Single(vm.Reviews);

        // The nest now reports the item closed — the VM must re-read rather
        // than assume, same as every other adjudication on this surface.
        rpc.NextMemberReviews = System.Array.Empty<FfiMemberReview>();
        var person = new byte[32];
        await vm.KeepAsync(person, null);

        Assert.Equal(person, rpc.LastMemberReviewKeep);
        Assert.Empty(vm.Reviews);
    }

    [Fact]
    public async Task RemoveAsync_CompleteEviction_ReReadsAndClearsTheRow()
    {
        var rpc = new MockNestRpcClient
        {
            NextMemberReviews = new[] { MockNestRpcClient.MakeMemberReview() },
            // Default NextMemberReviewRemoveResult is already complete (frees
            // nobody, earns Removed too — the ordinary deferred-backlog case).
        };
        var vm = new MemberReviewViewModel(rpc);
        await vm.LoadAsync(null);
        Assert.Single(vm.Reviews);

        rpc.NextMemberReviews = System.Array.Empty<FfiMemberReview>();
        await vm.RemoveAsync(new byte[32], NewManager());

        Assert.Empty(vm.Reviews);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task RemoveAsync_PartialEviction_TheRowStaysAndSaysWhy()
    {
        // The verdict is DERIVED: a partial eviction earns none, so the item
        // must stay open, never removed from the roster.
        var rpc = new MockNestRpcClient
        {
            NextMemberReviews = new[] { MockNestRpcClient.MakeMemberReview() },
            NextMemberReviewRemoveResult = new CrossGroupEviction(
                new[] { "t-1" },
                new[] { new EvictionFailure("t-2", "backend refused") },
                System.Array.Empty<UnreachableSeat>()),
        };
        var vm = new MemberReviewViewModel(rpc);
        await vm.LoadAsync(null);

        await vm.RemoveAsync(new byte[32], NewManager());

        // Never silently cleared — the row this test acted on is still there.
        Assert.Single(vm.Reviews);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }

    [Fact]
    public void ComposeRemovePartialMessage_NamesTheFolderAndUnsyncedSeatClasses()
    {
        // Rule (5)'s blocking classes each name the room their remedy lives
        // in — a blocked verdict with no named remedy reads as a button that
        // silently stopped working.
        var eviction = new CrossGroupEviction(
            System.Array.Empty<string>(),
            System.Array.Empty<EvictionFailure>(),
            new[]
            {
                new UnreachableSeat("f-1", UnreachableSeatClass.FolderChannel),
                new UnreachableSeat("c-1", UnreachableSeatClass.ChatGroupNoThreadHere),
            });

        var message = MemberReviewViewModel.ComposeRemovePartialMessage("alice", eviction);

        Assert.Contains(
            Strings.Get("settings/recovery_kit/review_remove_none_here").Replace("{who}", "alice"),
            message);
        Assert.Contains(
            Strings.Get("settings/recovery_kit/review_remove_folder_seats").Replace("{seats}", "1"),
            message);
        Assert.Contains(
            Strings.Get("settings/recovery_kit/review_remove_unsynced_seats").Replace("{seats}", "1"),
            message);
    }

    [Fact]
    public void ComposeRemovePartialMessage_NoUnreachableSeats_NamesNoRemedy()
    {
        // The no-failure lead lines cover the common case where nothing is
        // blocked by an unreachable seat at all.
        var eviction = new CrossGroupEviction(
            new[] { "t-1" },
            System.Array.Empty<EvictionFailure>(),
            System.Array.Empty<UnreachableSeat>());

        var message = MemberReviewViewModel.ComposeRemovePartialMessage("bob", eviction);

        Assert.Contains(
            Strings.Get("settings/recovery_kit/review_remove_done_here")
                .Replace("{who}", "bob").Replace("{removed}", "1"),
            message);
    }
}
