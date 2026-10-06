using System;
using System.Linq;
using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IMailAliasesMachine"/> for view-model unit tests — the
/// machine-as-seam peer of <see cref="MockNestRpcClient"/>. Records dispatched
/// actions (<see cref="Dispatched"/>) and returns a configurable
/// <see cref="NextSnapshot"/>; set <see cref="NextError"/> to make Dispatch/Hydrate
/// throw (error-path tests). The real shared machine captures errors into
/// <c>snapshot.error</c> and also throws, so a test exercising the error path can
/// set either or both.
/// </summary>
internal sealed class FakeMailAliasesMachine : MailAliasesMachineFakeBase
{
    public MailAliasesSnapshot NextSnapshot { get; set; } = Empty();
    public List<MailAliasesAction> Dispatched { get; } = new();
    public string? NextError { get; set; }
    public int HydrateCalls { get; private set; }

    public override Task Hydrate()
    {
        HydrateCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override Task Dispatch(MailAliasesAction action)
    {
        Dispatched.Add(action);
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override MailAliasesSnapshot Snapshot() => NextSnapshot;

    public static MailAliasesSnapshot Empty() =>
        new(Array.Empty<AliasView>(), null, null, null, AliasesStatus.Idle, null);

    public static MailAliasesSnapshot Snap(
        AliasView[] rows, string? domain = null, string? minted = null,
        ImportResultView? lastImportResult = null, string? error = null) =>
        new(rows, domain, minted, lastImportResult, AliasesStatus.Idle, error);

    public static AliasView Row(
        string idHex, AliasKind kind, string pattern, string address,
        string label = "", bool disabled = false, uint? spam = null, long? rate = null,
        ulong hits = 0, bool isCanonical = false, long? lastHitMs = null) =>
        new(idHex, "d.test", kind, pattern, address, label, disabled, isCanonical, hits,
            lastHitMs, spam, rate, null, null, null);
}

/// <summary>
/// Deterministic unit tests for the <c>mail-aliases</c> page VM, over the
/// <see cref="FakeMailAliasesMachine"/> (the UniFFI <c>IMailAliasesMachine</c> seam —
/// the e2e flow in test_mail_aliases.py made deterministic; no live nest / FlaUI,
/// which flakes on windows). Covers the snapshot projection (rows, default-domain
/// gating, minted-address surfacing) and that each user action dispatches the right
/// <c>MailAliasesAction</c> and re-reads the snapshot.
/// </summary>
public class MailAliasesViewModelTests
{
    private static AliasView[] SeededRows() => new[]
    {
        FakeMailAliasesMachine.Row("id-exact", AliasKind.Exact, "bob", "bob@d.test"),
        FakeMailAliasesMachine.Row("id-disp", AliasKind.Disposable, "Xy12Ab", "bob-temp-Xy12Ab@d.test"),
    };

    [Fact]
    public async Task Load_ProjectsRowsAndEnablesManage()
    {
        var fake = new FakeMailAliasesMachine
        {
            NextSnapshot = FakeMailAliasesMachine.Snap(SeededRows(), domain: "d.test"),
        };
        var vm = new MailAliasesViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        Assert.Equal(2, vm.Aliases.Count);
        Assert.Equal("bob@d.test", vm.Aliases[0].Address);
        Assert.Equal("id-exact", vm.Aliases[0].AliasIdHex);
        // Kind badge + hits text via the shared alias_kind_badge / alias_hits_label
        // formatters (no localizer in the test host → Strings.Resolve falls back to
        // the canonical dotted i18n key; a real localizer renders "Exact" / "0 hits").
        Assert.Equal("mail_aliases.kind_exact", vm.Aliases[0].KindBadge);
        Assert.Equal("mail_aliases.kind_disposable", vm.Aliases[1].KindBadge);
        // No last-hit timestamp on the seeded rows → the bare-count key.
        Assert.Equal("mail_aliases.hits", vm.Aliases[0].Hits);
        Assert.True(vm.CanManage);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task Load_HitsText_SelectsLastHitKeyWhenTimestampPresent()
    {
        // A row WITH a last-hit timestamp selects the shared with-last template
        // (alias_hits_label's Some-date branch, with the date formatted natively
        // in local time); a row WITHOUT one selects the bare-count key. No localizer
        // in the test host → the dotted key surfaces (a real localizer renders
        // "3 hits · last 2026-06-28" / "0 hits").
        var rows = new[]
        {
            FakeMailAliasesMachine.Row("id-hit", AliasKind.Wildcard, "bob-*", "bob-*@d.test",
                hits: 3, lastHitMs: 1_751_068_800_000),
            FakeMailAliasesMachine.Row("id-nohit", AliasKind.Exact, "bob", "bob@d.test"),
        };
        var fake = new FakeMailAliasesMachine
        {
            NextSnapshot = FakeMailAliasesMachine.Snap(rows, domain: "d.test"),
        };
        var vm = new MailAliasesViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("mail_aliases.hits_with_last", vm.Aliases[0].Hits);
        Assert.Equal("mail_aliases.hits", vm.Aliases[1].Hits);
    }

    [Fact]
    public async Task Load_NoDefaultDomain_DisablesManage()
    {
        var fake = new FakeMailAliasesMachine
        {
            // A user with no canonical exact alias → no default_domain.
            NextSnapshot = FakeMailAliasesMachine.Snap(Array.Empty<AliasView>(), domain: null),
        };
        var vm = new MailAliasesViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.CanManage);
        Assert.Empty(vm.Aliases);
    }

    [Fact]
    public async Task Create_DispatchesCreateWithFields()
    {
        var fake = new FakeMailAliasesMachine
        {
            NextSnapshot = FakeMailAliasesMachine.Snap(SeededRows(), domain: "d.test"),
        };
        var vm = new MailAliasesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.CreateAsync(wildcard: false, "shop", "Shopping", null, null);

        Assert.Contains(fake.Dispatched, a =>
            a is MailAliasesAction.Create { kind: AliasKind.Exact, pattern: "shop", label: "Shopping" });
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task GenerateDisposable_DispatchesAndSurfacesMintedAddress()
    {
        var fake = new FakeMailAliasesMachine
        {
            NextSnapshot = FakeMailAliasesMachine.Snap(SeededRows(), domain: "d.test"),
        };
        var vm = new MailAliasesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // The mint's success re-lists with last_minted_address set.
        fake.NextSnapshot = FakeMailAliasesMachine.Snap(
            SeededRows(), domain: "d.test", minted: "bob-temp-Zz99Qq@d.test");
        await vm.GenerateDisposableAsync();

        Assert.Contains(fake.Dispatched, a => a is MailAliasesAction.GenerateDisposable);
        Assert.Equal("bob-temp-Zz99Qq@d.test", vm.LastMintedAddress);
    }

    [Fact]
    public async Task Import_DispatchesLinesAndSurfacesResult()
    {
        var fake = new FakeMailAliasesMachine
        {
            NextSnapshot = FakeMailAliasesMachine.Snap(SeededRows(), domain: "d.test"),
        };
        var vm = new MailAliasesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // The import's success re-lists with last_import_result set (one line each of
        // created / skipped-duplicate / invalid — mail-aliases.md § Bulk import).
        var outcomes = new ImportAliasOutcomeView[]
        {
            new(0, "shop@d.test", ImportAliasStatusView.Created, null),
            new(1, "bob@d.test", ImportAliasStatusView.SkippedDuplicate, "already exists"),
            new(2, "not-an-email", ImportAliasStatusView.Invalid, "invalid address"),
        };
        fake.NextSnapshot = FakeMailAliasesMachine.Snap(
            SeededRows(), domain: "d.test",
            lastImportResult: new ImportResultView(1, 1, 1, outcomes));

        await vm.ImportAsync(new[] { "shop@d.test", "bob@d.test", "not-an-email" });

        Assert.Contains(fake.Dispatched, a =>
            a is MailAliasesAction.Import { lines: var lines }
            && lines.Length == 3
            && lines[0] == "shop@d.test");
        Assert.NotNull(vm.LastImportResult);
        Assert.Equal(1u, vm.LastImportResult!.Created);
        Assert.Equal(1u, vm.LastImportResult.SkippedDuplicate);
        Assert.Equal(1u, vm.LastImportResult.Invalid);
        Assert.Equal(3, vm.LastImportResult.Outcomes.Count);
        Assert.Equal("not-an-email", vm.LastImportResult.Outcomes[2].Address);
        Assert.Equal("Invalid", vm.LastImportResult.Outcomes[2].Status);
    }

    [Fact]
    public async Task RevokeThenDelete_DispatchTheRightActions()
    {
        var fake = new FakeMailAliasesMachine
        {
            NextSnapshot = FakeMailAliasesMachine.Snap(SeededRows(), domain: "d.test"),
        };
        var vm = new MailAliasesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.RevokeAsync("id-exact");
        await vm.DeleteAsync("id-disp");

        Assert.Contains(fake.Dispatched, a => a is MailAliasesAction.Revoke { aliasIdHex: "id-exact" });
        Assert.Contains(fake.Dispatched, a => a is MailAliasesAction.Delete { aliasIdHex: "id-disp" });
    }

    [Fact]
    public async Task Enable_DispatchesEnable()
    {
        var fake = new FakeMailAliasesMachine
        {
            NextSnapshot = FakeMailAliasesMachine.Snap(SeededRows(), domain: "d.test"),
        };
        var vm = new MailAliasesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // Re-enable a soft-off alias — the reverse of Revoke (the Active toggle's ON
        // direction), so disable is not a one-way trap (mail-aliases.md § Disable).
        await vm.EnableAsync("id-disp");

        Assert.Contains(fake.Dispatched, a => a is MailAliasesAction.Enable { aliasIdHex: "id-disp" });
    }

    [Fact]
    public async Task Load_ProjectsCanonicalAndActiveFlags()
    {
        var fake = new FakeMailAliasesMachine
        {
            NextSnapshot = FakeMailAliasesMachine.Snap(new[]
            {
                FakeMailAliasesMachine.Row("id-canon", AliasKind.Exact, "bob", "bob@d.test", isCanonical: true),
                FakeMailAliasesMachine.Row("id-extra", AliasKind.Exact, "shop", "shop@d.test", disabled: true),
            }, domain: "d.test"),
        };
        var vm = new MailAliasesViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        // The canonical row carries IsCanonical (drives the read-only render —
        // BoolToVisibilityInverse omits the mutating controls); ordinary rows do not.
        // mail-aliases.md:249.
        Assert.True(vm.Aliases[0].IsCanonical);
        Assert.False(vm.Aliases[1].IsCanonical);
        // Active is the toggle's IsOn (= !disabled): the canonical (enabled) row is
        // active, the disabled ordinary row is not.
        Assert.True(vm.Aliases[0].Active);
        Assert.False(vm.Aliases[1].Active);
    }

    [Fact]
    public async Task Dispatch_Failure_RoutesToError()
    {
        var fake = new FakeMailAliasesMachine
        {
            NextSnapshot = FakeMailAliasesMachine.Snap(SeededRows(), domain: "d.test"),
        };
        var vm = new MailAliasesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // The machine throws (and, in production, also captures snapshot.error). The VM
        // swallows the throw and surfaces the error rather than letting it escape.
        fake.NextError = "boom";
        await vm.CreateAsync(wildcard: false, "x", "", null, null);

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }
}
