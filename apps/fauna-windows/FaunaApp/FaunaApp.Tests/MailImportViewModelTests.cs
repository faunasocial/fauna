using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IMailImportMachine"/> for view-model unit tests: hands back one
/// configurable snapshot and records nothing else.
/// </summary>
internal sealed class FakeMailImportMachine : MailImportMachineFakeBase
{
    public MailImportSnapshot NextSnapshot { get; set; } = Empty();

    public override Task Hydrate() => Task.CompletedTask;

    public override MailImportSnapshot Snapshot() => NextSnapshot;

    public static MailImportSnapshot Empty(
        ImportStep step = ImportStep.Source,
        SourceMailboxOption[]? mailboxes = null,
        ImportMailboxProgress[]? progress = null) =>
        new(step, ImportSourceKind.Gmail, "", 993, ImportTlsMode.Implicit, "", "",
            mailboxes ?? Array.Empty<SourceMailboxOption>(), "", 0, null, 0, 0, 0, 0,
            Array.Empty<string>(), ImportStatus.Idle, null,
            mailboxProgress: progress ?? Array.Empty<ImportMailboxProgress>(),
            rebuiltMailboxes: Array.Empty<string>());
}

/// <summary>
/// The Confirm total and the Progress rows of the <c>mail-import</c> wizard
/// (<c>mailbox-migration.md</c> § Wizard steps steps 4 and 5): both come from the shared
/// machine's own readings, never from a sum or a planned count composed in the view model.
/// Shares the process-global <see cref="Strings"/> localizer, hence the serialized collection.
/// </summary>
[Collection("StringsGlobal")]
public class MailImportViewModelTests
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
            ["mail_import/confirm_summary_fmt"] = "{source} · {mailboxes} mailbox(es) · {messages} messages",
            ["mail_import/confirm_summary_sized_fmt"] =
                "{source} · {mailboxes} mailbox(es) · {messages} messages · about {size}",
            ["mail_import/progress_row_counts_fmt"] =
                "{imported} of {count} · {skipped} skipped · {errored} errored",
            // The planned-count template must never paint a Progress row any more.
            ["mail_import/progress_row_fmt"] = "PLANNED {count}",
        }));

    private static async Task<MailImportViewModel> Loaded(MailImportSnapshot snap)
    {
        var vm = new MailImportViewModel(new FakeMailImportMachine { NextSnapshot = snap });
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.Null(vm.Error);
        return vm;
    }

    [Fact]
    public async Task Confirm_states_the_shared_total_and_a_size_once_every_selected_mailbox_has_one()
    {
        UseLocalizer();
        var vm = await Loaded(FakeMailImportMachine.Empty(ImportStep.Confirm, new[]
        {
            new SourceMailboxOption("INBOX", true, 3, 1_000UL),
            new SourceMailboxOption("Sent", true, 4, 2_000UL),
            new SourceMailboxOption("Trash", false, 99, 9_999UL),
        }));

        // 2 selected mailboxes, 7 messages — the unselected Trash is not counted.
        Assert.Contains("2 mailbox(es) · 7 messages · about ", vm.ConfirmSummary);
    }

    [Fact]
    public async Task Confirm_falls_back_to_the_count_only_summary_when_a_selected_mailbox_has_no_size()
    {
        UseLocalizer();
        var vm = await Loaded(FakeMailImportMachine.Empty(ImportStep.Confirm, new[]
        {
            new SourceMailboxOption("INBOX", true, 3, 1_000UL),
            new SourceMailboxOption("Sent", true, 4, null),
        }));

        Assert.EndsWith("2 mailbox(es) · 7 messages", vm.ConfirmSummary);
        Assert.DoesNotContain("about", vm.ConfirmSummary);
    }

    [Fact]
    public async Task Progress_rows_come_from_the_snapshots_mailbox_progress_not_the_planned_count()
    {
        UseLocalizer();
        var vm = await Loaded(FakeMailImportMachine.Empty(
            ImportStep.Progress,
            mailboxes: new[] { new SourceMailboxOption("INBOX", true, 5, null) },
            progress: new[] { new ImportMailboxProgress("INBOX", 2, 1, 0, 5) }));

        var row = Assert.Single(vm.MailboxProgress);
        Assert.Equal("INBOX", row.Name);
        Assert.Equal("2 of 5 · 1 skipped · 0 errored", row.Progress);
        Assert.DoesNotContain("PLANNED", row.Progress);
    }

    [Fact]
    public async Task Max_size_draft_seed_is_the_machines_own_value_in_whole_megabytes()
    {
        // § Wizard steps step 3: the size limit starts at the machine's 50 MiB default;
        // a lowered limit a previous Next committed is the snapshot's value thereafter.
        UseLocalizer();
        var fresh = await Loaded(FakeMailImportMachine.Empty(ImportStep.Scope)
            with { maxSizeBytes = 50UL * 1024 * 1024 });
        Assert.Equal("50", fresh.MaxSizeMb);

        var lowered = await Loaded(FakeMailImportMachine.Empty(ImportStep.Scope)
            with { maxSizeBytes = 10UL * 1024 * 1024 });
        Assert.Equal("10", lowered.MaxSizeMb);
    }

    [Fact]
    public async Task Progress_rows_are_empty_before_a_start_laid_them()
    {
        UseLocalizer();
        var vm = await Loaded(FakeMailImportMachine.Empty(
            ImportStep.Progress,
            mailboxes: new[] { new SourceMailboxOption("INBOX", true, 5, null) }));

        Assert.Empty(vm.MailboxProgress);
    }
}
