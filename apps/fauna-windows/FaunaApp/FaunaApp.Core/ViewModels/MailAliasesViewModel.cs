using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.ViewModels;

/// <summary>One imported line's outcome (mail-aliases.md § Bulk import). Mirrors the
/// internal <see cref="ImportAliasOutcomeView"/> 1:1 — <c>Status</c> is the enum's
/// display name (<c>"Created"</c>/<c>"SkippedDuplicate"</c>/<c>"Invalid"</c>) rather
/// than the internal <c>ImportAliasStatusView</c> type, so the public mirror stays
/// decoupled (the assembly-boundary rule — see <see cref="SpamPolicyValues"/> in
/// AdminMailViewModel.cs).</summary>
public sealed record MailAliasImportOutcome(uint LineIndex, string Address, string Status, string? Reason)
{
    internal static MailAliasImportOutcome From(ImportAliasOutcomeView v) =>
        new(v.lineIndex, v.address, v.status.ToString(), v.reason);
}

/// <summary>Read-only outcome of the last <c>Import</c> action
/// (<c>mail-aliases-import-result</c>). Mirrors the internal <see cref="ImportResultView"/>;
/// the summary counts render via the shared <c>mail_aliases.import_result</c> template,
/// substituted in <c>MailAliasesPanel.xaml.cs</c> (windows resw is flat — no named-placeholder
/// support server-side; mirrors AdminMailPage's <c>BaselinePublishValues</c> rendering).</summary>
public sealed record MailAliasImportResult(
    uint Created, uint SkippedDuplicate, uint Invalid, IReadOnlyList<MailAliasImportOutcome> Outcomes)
{
    internal static MailAliasImportResult From(ImportResultView v) => new(
        v.created, v.skippedDuplicate, v.invalid, v.outcomes.Select(MailAliasImportOutcome.From).ToList());
}

/// <summary>
/// One alias row (<c>mail-aliases-list-item</c>), projected from the shared
/// machine's <see cref="AliasView"/>. Carries both the display strings the
/// DataTemplate binds and the raw editable fields the Edit sheet pre-populates.
/// </summary>
public sealed class MailAliasRow
{
    /// <summary>Lowercase hex alias id — the per-row update/revoke/delete key.</summary>
    public required string AliasIdHex { get; init; }
    /// <summary>The kind-aware display address (<c>mail-aliases-list-item-pattern</c>).</summary>
    public required string Address { get; init; }
    /// <summary>Whether this alias is a wildcard prefix — drives the Edit-sheet picker
    /// (read-only on edit; kind is immutable). A primitive, so the public row API stays
    /// decoupled from the UniFFI-internal <c>AliasKind</c>.</summary>
    public required bool IsWildcard { get; init; }
    /// <summary>The raw wire pattern (localpart / prefix) — Edit-sheet pre-populate.</summary>
    public required string Pattern { get; init; }
    public required string KindBadge { get; init; }
    public required string Label { get; init; }
    public required string Hits { get; init; }
    public required bool Disabled { get; init; }
    /// <summary>The two-way "Active" toggle's <c>IsOn</c>: ON = receiving (enabled),
    /// OFF = disabled. Toggling drives Enable / Revoke (mail-aliases.md § Disable —
    /// both directions exist so disable is not a one-way unrecoverable trap).</summary>
    public bool Active => !Disabled;
    /// <summary>Whether this is the canonical <c>&lt;handle&gt;@&lt;domain&gt;</c> primary
    /// alias (mail-aliases.md:249, wire <c>AliasView.is_canonical</c>). When set the row
    /// renders read-only — the mutating controls (toggle / edit / revoke / delete) are
    /// omitted and a "primary address" marker shown — because the nest rejects disabling /
    /// renaming / deleting it (<c>canonical_alias_protected</c>).</summary>
    public required bool IsCanonical { get; init; }
    public uint? SpamThresholdOverride { get; init; }
    public long? RateLimitPerHour { get; init; }
    public required string EditLabel { get; init; }
    public required string RevokeLabel { get; init; }
    public required string DeleteLabel { get; init; }
    public required string ShowAuditLabel { get; init; }

    internal static MailAliasRow From(AliasView a) => new()
    {
        AliasIdHex = a.aliasIdHex,
        Address = a.address,
        IsWildcard = a.kind == AliasKind.Wildcard,
        Pattern = a.pattern,
        KindBadge = Strings.Resolve(FaunaClientMailSettingsMethods.AliasKindBadge(a.kind)),
        Label = a.label,
        Hits = Strings.Resolve(FaunaClientMailSettingsMethods.AliasHitsLabel(
            a.hitCount, a.lastHitAtMs is long ms ? FormatMillisLocal(ms) : null)),
        Disabled = a.disabled,
        IsCanonical = a.isCanonical,
        SpamThresholdOverride = a.spamThresholdOverride,
        RateLimitPerHour = a.rateLimitPerHour,
        EditLabel = Strings.Get("mail_aliases/edit"),
        RevokeLabel = Strings.Get("mail_aliases/revoke"),
        DeleteLabel = Strings.Get("mail_aliases/delete"),
        ShowAuditLabel = Strings.Get("mail_aliases/show_audit"),
    };

    /// <summary>The <c>mail-aliases-list-item-hits</c> last-hit date — the shared
    /// local YYYY-MM-DD render via its ms door (value-formatting.md § Absolute
    /// local timestamp display; the surrounding "{count} hits · last {date}"
    /// template is shared via
    /// <c>fauna_client_mail_settings::alias_hits_label</c>).</summary>
    private static string FormatMillisLocal(long ms) =>
        uniffi.fauna_ffi.FaunaFfiMethods.FormatUnixLocalDateMs(ms);
}

/// <summary>
/// The user-facing <c>mail-aliases</c> page (docs/goal/behavior/mail-aliases.md
/// § Aliases UX) — a person managing their own per-account mail addresses. A dumb
/// projection over the shared <c>fauna_client_mail_settings::MailAliasesMachine</c>,
/// consumed through its UniFFI-generated <see cref="IMailAliasesMachine"/> interface
/// (the machine-as-seam — no hand-written seam; the page builds the real machine,
/// the unit test fakes the interface). All business logic (validation,
/// <c>default_domain</c> derivation, action sequencing) lives in shared Rust
/// (priority #2); this VM forwards the five user actions and re-projects the snapshot
/// after each. Mirrors <see cref="AdminNestViewModel"/> (the reflective-over-seam analog);
/// lifts the linux reference apps/fauna-linux/src/settings/mail_aliases.rs.
///
/// The per-row disable toggle is two-way (Revoke + Enable) — disable is no longer a
/// one-way trap (mail-aliases.md § Disable, a product invariant). One honest gap remains,
/// mirrored from the lead (surfaced, never faked): the per-row Show-audit disclosure is
/// inert (list_account_alias_hits is a follow-on slice). The page renders it honestly.
/// </summary>
public partial class MailAliasesViewModel : ObservableObject
{
    private readonly IMailAliasesMachine _machine;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    /// <summary>Whether add / generate are available: the user has a canonical exact
    /// alias so the machine derived a <c>default_domain</c> (mail-aliases.md § Kind 1).
    /// Without one the nest rejects create/mint with <c>no_canonical_address</c>.</summary>
    [ObservableProperty] private bool _canManage;

    /// <summary>Set after a successful disposable mint to the full minted address — the
    /// page copies it to the clipboard + toasts. Cleared on the next dispatch.</summary>
    [ObservableProperty] private string? _lastMintedAddress;

    /// <summary>Set after a successful <see cref="ImportAsync"/> to the per-line outcome
    /// summary (<c>mail-aliases-import-result</c>) — the page renders the
    /// <c>mail_aliases.import_result</c> template from the counts. Cleared on the next
    /// dispatch (mirrors <see cref="LastMintedAddress"/>).</summary>
    [ObservableProperty] private MailAliasImportResult? _lastImportResult;

    /// <summary>The caller's own aliases (one <c>mail-aliases-list</c> row each), rebuilt
    /// from the snapshot on every projection.</summary>
    public ObservableCollection<MailAliasRow> Aliases { get; } = new();

    /// <summary>True once a <see cref="LoadAsync"/> round trip has actually completed —
    /// the loading-is-not-empty gate (`ui/README.md` § List pages: loading is not empty;
    /// rule-5 render lift). An empty <see cref="Aliases"/> pre-hydrate must NOT read as
    /// "no aliases"; only <c>Loaded &amp;&amp; Aliases.Count == 0</c> means that. Never set
    /// on a failed load — only a completed round trip flips it.</summary>
    [ObservableProperty] private bool _loaded;

    internal MailAliasesViewModel(IMailAliasesMachine machine)
    {
        _machine = machine;
    }

    /// <summary>Initial page load: hydrate the alias list, then project the snapshot.
    /// The transport already tolerates the post-login connect race for a single RPC
    /// (transport.md § Request lifecycle step 3) — no app-level retry needed here.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            await _machine.Hydrate();
            Apply(_machine.Snapshot());
            Loaded = true;
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Create an Exact (<paramref name="wildcard"/> false) or Wildcard-prefix
    /// alias on the snapshot's <c>default_domain</c>. The machine pre-validates the pattern
    /// + the nest re-validates / enforces cross-user uniqueness; a failure surfaces in
    /// <see cref="Error"/>. Takes a <c>bool</c> rather than the UniFFI-internal
    /// <c>AliasKind</c> so the public VM API stays decoupled.</summary>
    public Task CreateAsync(bool wildcard, string pattern, string label, uint? spamThresholdOverride, long? rateLimitPerHour)
        => DispatchAsync(new MailAliasesAction.Create(
            wildcard ? AliasKind.Wildcard : AliasKind.Exact, pattern, label, spamThresholdOverride, rateLimitPerHour));

    /// <summary>Mint a disposable alias with the per-user defaults (the nest derives
    /// <c>&lt;handle&gt;+&lt;domain&gt;</c> server-side from the canonical exact alias).</summary>
    public Task GenerateDisposableAsync()
        => DispatchAsync(new MailAliasesAction.GenerateDisposable(null, null, string.Empty));

    /// <summary>Full-overwrite an owned alias's editable fields (kind is immutable).</summary>
    public Task UpdateAsync(string aliasIdHex, string pattern, string label, uint? spamThresholdOverride, long? rateLimitPerHour)
        => DispatchAsync(new MailAliasesAction.Update(aliasIdHex, pattern, label, spamThresholdOverride, rateLimitPerHour));

    /// <summary>Soft-off an owned alias (flip <c>disabled = true</c>; row preserved).</summary>
    public Task RevokeAsync(string aliasIdHex)
        => DispatchAsync(new MailAliasesAction.Revoke(aliasIdHex));

    /// <summary>Re-enable a soft-off alias (flip <c>disabled = false</c>; the reverse of
    /// <see cref="RevokeAsync"/>). Backs the Active toggle's on direction so disable is
    /// not a one-way trap (mail-aliases.md § Disable — a product invariant).</summary>
    public Task EnableAsync(string aliasIdHex)
        => DispatchAsync(new MailAliasesAction.Enable(aliasIdHex));

    /// <summary>Destructively remove an owned alias (irreversible).</summary>
    public Task DeleteAsync(string aliasIdHex)
        => DispatchAsync(new MailAliasesAction.Delete(aliasIdHex));

    /// <summary>Bulk-import exact aliases from pasted lines (<c>mail-aliases-import-submit-button</c>
    /// → mail-aliases.md § Bulk import). Best-effort per line; the machine derives one exact
    /// alias per non-blank line, skipping duplicates/invalid addresses. The outcome lands in
    /// <see cref="LastImportResult"/> and the list re-lists.</summary>
    public Task ImportAsync(IReadOnlyList<string> lines)
        => DispatchAsync(new MailAliasesAction.Import(lines.ToArray()));

    /// <summary>Dispatch an action then re-project the snapshot. The machine captures any
    /// user-facing error into <c>snapshot.error</c> (and also throws), so the throw is
    /// swallowed and the error read from the snapshot — matching MailSettingsPanel / linux;
    /// the exception is a fallback only if the snapshot carried no error.</summary>
    private async Task DispatchAsync(MailAliasesAction action)
    {
        try
        {
            await _machine.Dispatch(action);
            Apply(_machine.Snapshot());
        }
        catch (Exception ex)
        {
            Apply(_machine.Snapshot());
            if (string.IsNullOrEmpty(Error)) Error = Strings.Error(ex);
        }
    }

    private void Apply(MailAliasesSnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        CanManage = snap.defaultDomain is not null;
        LastMintedAddress = snap.lastMintedAddress;
        LastImportResult = snap.lastImportResult is null ? null : MailAliasImportResult.From(snap.lastImportResult);
        Aliases.Clear();
        foreach (var a in snap.aliases)
        {
            Aliases.Add(MailAliasRow.From(a));
        }
    }
}
