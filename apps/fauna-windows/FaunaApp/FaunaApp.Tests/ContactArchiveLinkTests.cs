using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_archive_import_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IArchiveLinks"/>: hands back the configured views and records the
/// shared calls the Contacts row and the host issue.
/// </summary>
internal sealed class FakeArchiveLinks : ArchiveLinksFakeBase
{
    public ArchiveLinkView[] NextViews { get; set; } = Array.Empty<ArchiveLinkView>();
    public bool WorkDue { get; set; }
    public ulong Captures { get; set; }
    public int Drives { get; private set; }
    public int ViewReads { get; private set; }
    public List<(string Contact, string Platform, bool Yes)> Answers { get; } = new();
    public List<(string Contact, string Platform)> Unlinks { get; } = new();

    public override Task<ArchiveLinkView[]> LinkViews()
    {
        ViewReads++;
        return Task.FromResult(NextViews);
    }

    public override Task AnswerLinkToken(string contact, string platform, bool yes)
    {
        Answers.Add((contact, platform, yes));
        return Task.CompletedTask;
    }

    public override Task UnlinkToken(string contact, string platform)
    {
        Unlinks.Add((contact, platform));
        return Task.CompletedTask;
    }

    public override Task DriveLinks()
    {
        Drives++;
        return Task.CompletedTask;
    }

    public override bool LinkWorkDue() => WorkDue;

    public override ulong LinkCaptures() => Captures;
}

/// <summary>
/// Phase two of archive import on the windows Contacts row
/// (<c>archive-import.md</c> § Linking and merging, ruling 8; <c>contacts.md</c> § User
/// actions) — the windows twin of tui's
/// <c>contacts::tests::the_archive_link_prompt_renders_only_on_its_asked_row_and_unlink_only_when_linked</c>.
/// The state is the shared <see cref="ArchiveLinkView"/> and the words the machine crate's
/// real composers (<c>archive_link_prompt</c> / <c>archive_linked_label</c>, through the
/// native dll), so what is pinned here is the windows wiring: which row paints what,
/// and that the row's taps reach the shared calls with the row's own contact and platform.
/// Shares the process-global <see cref="Strings"/> localizer, hence the serialized collection.
/// </summary>
[Collection("StringsGlobal")]
public class ContactArchiveLinkTests
{
    private static readonly string A = "aa".PadRight(64, 'a');
    private static readonly string B = "bb".PadRight(64, 'b');
    private static readonly string C = "cc".PadRight(64, 'c');

    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    // The en.yaml wording, as Resources.resw carries it.
    private static void UseLocalizer() =>
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["archive_import/link_prompt_fmt"] = "Is {contact} the “{archived_name}” from your {platform} friends?",
            ["archive_import/link_prompt_unnamed_fmt"] = "Is {contact} one of your {platform} friends?",
            ["archive_import/linked_fmt"] = "Linked: {platform}",
            ["archive_import/linked_unmatched_fmt"] = "Linked: {platform} · {unmatched} could not be matched to a post",
        }));

    private static ArchiveLinkView View(string contact, ArchiveLinkState state, ulong unmatched = 0) =>
        new(contact, state, "facebook", "Friend Two", 0, unmatched);

    private static ContactInfo Contact(string actorId, string handle, ContactStatus status) =>
        new(actorId, handle, status, null);

    /// <summary>A loaded Contacts page over <paramref name="links"/>: the host started on
    /// them (no timer — the test drives its ticks), the roster read through the mock.</summary>
    private static async Task<(ContactsViewModel Vm, ArchiveLinksHost Host)> Loaded(FakeArchiveLinks? links)
    {
        var rpc = new MockNestRpcClient
        {
            NextContacts = new[]
            {
                Contact(A, "ada", ContactStatus.Accepted),
                Contact(B, "bo", ContactStatus.Accepted),
                Contact(C, "cy", ContactStatus.Confirmed),
            },
        };
        var host = new ArchiveLinksHost(tickInterval: null);
        if (links is not null)
        {
            host.Start(links);
            await host.LastDrive;
        }
        var vm = new ContactsViewModel(rpc, host);
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.Null(vm.ErrorMessage);
        return (vm, host);
    }

    /// <summary>The question and its yes / no only on an asked row, the linked line and
    /// unlink only on a linked one, nothing for the other states — each on its own row,
    /// the asked contact deliberately SECOND so a paint that ignored the contact would
    /// land on the wrong person.</summary>
    [Fact]
    public async Task The_archive_link_prompt_renders_only_on_its_asked_row_and_unlink_only_when_linked()
    {
        UseLocalizer();
        var links = new FakeArchiveLinks
        {
            NextViews = new[]
            {
                View(A, ArchiveLinkState.Offered),
                View(B, ArchiveLinkState.Asked),
                View(C, ArchiveLinkState.Linked, unmatched: 2),
            },
        };
        var (vm, host) = await Loaded(links);

        Assert.Equal(new[] { A, B, C }, vm.Rows.Select(r => r.ActorId).ToArray());
        Assert.Empty(vm.Rows[0].ArchiveLinks);

        var asked = Assert.Single(vm.Rows[1].ArchiveLinks);
        Assert.Equal(ArchiveLinkRowKind.Asked, asked.Kind);
        Assert.True(asked.IsAsked);
        Assert.False(asked.IsLinked);
        Assert.Equal("Is bo the “Friend Two” from your Facebook friends?", asked.Text);
        Assert.Equal((B, "facebook"), (asked.Contact, asked.Platform));

        var linked = Assert.Single(vm.Rows[2].ArchiveLinks);
        Assert.Equal(ArchiveLinkRowKind.Linked, linked.Kind);
        Assert.True(linked.IsLinked);
        Assert.False(linked.IsAsked);
        Assert.Equal("Linked: Facebook · 2 could not be matched to a post", linked.Text);

        // Declined and unlinked rows ask nothing and offer nothing — repainted when the
        // host's views move, with no roster reload.
        links.NextViews = new[]
        {
            View(B, ArchiveLinkState.Declined),
            View(C, ArchiveLinkState.Unlinked),
        };
        await host.RefreshAsync();
        Assert.True(vm.RepaintArchiveLinksIfMoved());
        Assert.All(vm.Rows, r => Assert.Empty(r.ArchiveLinks));
        Assert.False(vm.RepaintArchiveLinksIfMoved());
    }

    /// <summary>The row's yes / no / unlink carry the row's own contact and platform into
    /// the shared calls; with no links wired they issue nothing and paint nothing.</summary>
    [Fact]
    public async Task The_archive_link_answers_dispatch_the_shared_calls_for_their_row()
    {
        UseLocalizer();
        var links = new FakeArchiveLinks
        {
            NextViews = new[] { View(B, ArchiveLinkState.Asked), View(C, ArchiveLinkState.Linked) },
        };
        var (vm, _) = await Loaded(links);

        var asked = Assert.Single(vm.Rows[1].ArchiveLinks);
        await vm.AnswerArchiveLinkAsync(asked, yes: false);
        Assert.Equal(new[] { (B, "facebook", false) }, links.Answers.ToArray());

        // The answer re-reads the views: the shared state moved to linked, so the row
        // drops the question and offers unlink.
        links.NextViews = new[] { View(B, ArchiveLinkState.Linked), View(C, ArchiveLinkState.Linked) };
        await vm.AnswerArchiveLinkAsync(asked, yes: true);
        Assert.Equal((B, "facebook", true), links.Answers[^1]);
        var nowLinked = Assert.Single(vm.Rows[1].ArchiveLinks);
        Assert.Equal(ArchiveLinkRowKind.Linked, nowLinked.Kind);

        await vm.UnlinkArchiveLinkAsync(Assert.Single(vm.Rows[2].ArchiveLinks));
        Assert.Equal(new[] { (C, "facebook") }, links.Unlinks.ToArray());

        // No links wired: no paint, and a stale paint's tap reaches nothing.
        var (bare, _) = await Loaded(null);
        Assert.All(bare.Rows, r => Assert.Empty(r.ArchiveLinks));
        var before = links.Answers.Count;
        await bare.AnswerArchiveLinkAsync(asked, yes: true);
        await bare.UnlinkArchiveLinkAsync(asked);
        Assert.Equal(before, links.Answers.Count);
        Assert.Single(links.Unlinks);
    }

    /// <summary>The login hand-over (archive-import.md § Implementation status today, the
    /// tui shape every app inherits): one drive at once, then a tick that drives again
    /// only while work is due and re-reads the views only when the sink captured
    /// something — both atomic loads, so an idle tick reads no folder.</summary>
    [Fact]
    public async Task Start_drives_once_and_the_tick_drives_on_work_due_and_rereads_on_captures()
    {
        UseLocalizer();
        var links = new FakeArchiveLinks { Captures = 7 };
        var host = new ArchiveLinksHost(tickInterval: null);
        var changes = 0;
        host.Changed += () => changes++;

        host.Start(links);
        await host.LastDrive;
        Assert.Equal(1, links.Drives);
        Assert.Same(links, host.Current);
        var readsAfterStart = links.ViewReads;

        // Idle tick: nothing due, nothing captured — no drive, no read.
        await host.TickAsync();
        await host.LastDrive;
        Assert.Equal(1, links.Drives);
        Assert.Equal(readsAfterStart, links.ViewReads);

        // Work due → a drive (which re-reads when it ends).
        links.WorkDue = true;
        await host.TickAsync();
        await host.LastDrive;
        Assert.Equal(2, links.Drives);
        links.WorkDue = false;

        // A question captured by the sink → the views are re-read and Changed fires.
        links.NextViews = new[] { View(B, ArchiveLinkState.Asked) };
        links.Captures = 8;
        var reads = links.ViewReads;
        await host.TickAsync();
        Assert.Equal(reads + 1, links.ViewReads);
        Assert.Equal(B, Assert.Single(host.Views).contact);
        Assert.True(changes > 0);

        // The same links again is a no-op, not a second drive.
        host.Start(links);
        await host.LastDrive;
        Assert.Equal(2, links.Drives);
    }

    /// <summary>An actor change stops the host (ActorScope.DropActorScopedState): the
    /// views go, so the incoming account's rows paint none of the outgoing one's
    /// questions, and the answer path reaches nothing.</summary>
    [Fact]
    public async Task Stop_drops_the_views_and_the_links()
    {
        UseLocalizer();
        var links = new FakeArchiveLinks { NextViews = new[] { View(B, ArchiveLinkState.Asked) } };
        var host = new ArchiveLinksHost(tickInterval: null);
        host.Start(links);
        await host.LastDrive;
        Assert.Single(host.Views);
        var revision = host.Revision;

        host.Stop();

        Assert.Null(host.Current);
        Assert.Empty(host.Views);
        Assert.NotEqual(revision, host.Revision);
        await host.AnswerAsync(B, "facebook", true);
        Assert.Empty(links.Answers);
    }
}
