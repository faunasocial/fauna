using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_archive_import_machine;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IArchiveImportMachine"/> for view-model unit tests: hands back one
/// configurable snapshot and records the dispatched actions.
/// </summary>
internal sealed class FakeArchiveImportMachine : ArchiveImportMachineFakeBase
{
    public ArchiveImportSnapshot NextSnapshot { get; set; } = Done();
    public List<ArchiveImportAction> Dispatched { get; } = new();

    public override Task Hydrate() => Task.CompletedTask;

    public override Task Dispatch(ArchiveImportAction action)
    {
        Dispatched.Add(action);
        return Task.CompletedTask;
    }

    public override ArchiveImportSnapshot Snapshot() => NextSnapshot;

    public static ArchiveImportSnapshot Done(
        ulong commentsUnplaced = 0,
        ArchiveProfileView? profile = null,
        bool profileApplied = false) =>
        new(ArchiveImportStep.Done, ArchiveSourceKind.Facebook, "", "", null,
            Array.Empty<CategoryRow>(), AudienceMode.Original, "", "", 0, 0, "archive-folder",
            RunState.Completed, 5, 4, 1, 0, null, 10, null, Array.Empty<string>(), true, false,
            ArchiveImportStatus.Idle, null, 0, 0,
            commentsUnplaced: commentsUnplaced, archiveProfile: profile, profileApplied: profileApplied);
}

/// <summary>
/// The <c>archive-import</c> page's Done step (<c>archive-import.md</c> § Implementation
/// status today): every line comes from the shared <c>archive_import_page_text</c> composers,
/// and the profile prefill is offered only while an archive profile is set and not yet applied.
/// Shares the process-global <see cref="Strings"/> localizer, hence the serialized collection.
/// </summary>
[Collection("StringsGlobal")]
public class ArchiveImportViewModelTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    private static void UseLocalizer() =>
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["errors/http_error"] = "HTTP error: {0}",
            ["archive_import/done_summary_fmt"] = "{imported} imported · {skipped} skipped",
            ["archive_import/done_summary_unplaced_fmt"] =
                "{imported} imported · {skipped} skipped · {count} comments could not be placed under a post",
        }));

    private static async Task<(ArchiveImportViewModel Vm, FakeArchiveImportMachine Machine)> Loaded(
        ArchiveImportSnapshot snap)
    {
        var machine = new FakeArchiveImportMachine { NextSnapshot = snap };
        var vm = new ArchiveImportViewModel(machine);
        await vm.LoadAsync();
        Assert.Null(vm.Error);
        return (vm, machine);
    }

    [Fact]
    public async Task Done_line_is_the_shared_composition_without_the_unplaced_clause_when_none()
    {
        UseLocalizer();
        var (vm, _) = await Loaded(FakeArchiveImportMachine.Done());

        Assert.Equal("4 imported · 1 skipped", vm.DoneSummary);
    }

    [Fact]
    public async Task Done_line_carries_the_unplaced_comments_clause_from_the_shared_composer()
    {
        UseLocalizer();
        var (vm, _) = await Loaded(FakeArchiveImportMachine.Done(commentsUnplaced: 3));

        Assert.Equal(
            "4 imported · 1 skipped · 3 comments could not be placed under a post", vm.DoneSummary);
    }

    [Fact]
    public async Task Profile_prefill_is_offered_only_while_a_profile_is_set_and_not_applied()
    {
        UseLocalizer();
        var profile = new ArchiveProfileView("Ada", null, Array.Empty<string>(), false);

        var (none, _) = await Loaded(FakeArchiveImportMachine.Done());
        Assert.False(none.ProfilePrefillAvailable);

        var (offered, _) = await Loaded(FakeArchiveImportMachine.Done(profile: profile));
        Assert.True(offered.ProfilePrefillAvailable);

        var (applied, _) = await Loaded(FakeArchiveImportMachine.Done(profile: profile, profileApplied: true));
        Assert.False(applied.ProfilePrefillAvailable);
    }

    [Fact]
    public async Task Profile_prefill_click_dispatches_apply_archive_profile()
    {
        UseLocalizer();
        var profile = new ArchiveProfileView("Ada", null, Array.Empty<string>(), false);
        var (vm, machine) = await Loaded(FakeArchiveImportMachine.Done(profile: profile));

        await vm.ApplyProfileAsync();

        Assert.Contains(machine.Dispatched, a => a is ArchiveImportAction.ApplyArchiveProfile);
    }
}
