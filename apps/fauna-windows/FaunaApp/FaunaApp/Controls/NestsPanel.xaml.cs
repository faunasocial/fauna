using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using FaunaApp.Helpers;
using uniffi.fauna_ffi;
using uniffi.fauna_client_pair;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Controls;

/// <summary>
/// One current content-processing grant (<c>nest-trust-grant-item</c>), OneTime
/// x:Bind-projected from <c>TrustGrantRow</c> — scope, lasts-until, liveness
/// status, the required honest-bound copy, and the renew/revoke button labels.
/// <see cref="GrantId"/> round-trips unchanged into the Renew/Revoke dispatch.
/// </summary>
public sealed class TrustGrantItem
{
    public byte[] GrantId { get; init; } = Array.Empty<byte>();
    public string Scope { get; init; } = string.Empty;
    public string LastsUntil { get; init; } = string.Empty;
    public string Status { get; init; } = string.Empty;
    public string BoundNote { get; init; } = string.Empty;
    public string RenewLabel { get; init; } = string.Empty;
    public string RevokeLabel { get; init; } = string.Empty;
    /// <summary>The post-succession review pair's gate — Visible only on a grant
    /// the shared projection marks <c>unattested</c> (<c>TrustGrantRow.unattested</c>,
    /// joined once in shared Rust), Collapsed (absent to UIA) otherwise.</summary>
    public Visibility UnattestedVisibility { get; init; } = Visibility.Collapsed;
    public string UnattestedMark { get; init; } = string.Empty;
    public string KeepLabel { get; init; } = string.Empty;
}

/// <summary>
/// One backup trust row (<c>nest-trust-backup-item</c>) on the home nest's row —
/// the owner→source-nest <c>NestBackupKey</c> seal grant, or one row per backup
/// destination for the source nest's writer authorization there. OneTime x:Bind-
/// projected from <c>TrustBackupRow</c>; <see cref="Kind"/> and
/// <see cref="DestinationId"/> round-trip unchanged into the revoke dispatch
/// (<c>RevokeBackup_Click</c>), which is why the whole item — not just an id —
/// is bound onto the revoke button's <c>Tag</c> (<c>Tag="{x:Bind}"</c>, the same
/// shape <c>BackupsPage.xaml</c> already uses).
/// </summary>
public sealed class TrustBackupItem
{
    // internal, not public: TrustBackupKind is a UniFFI-internal type (CS0053 on
    // a public member — same precedent as NestRowItem.MintOptions above). Never
    // XAML-bound (x:Bind never touches it); only RevokeBackup_Click reads it.
    internal TrustBackupKind Kind { get; init; }
    /// <summary><c>BackupDestination::destination_id</c> on a Writer row; empty on Seal.</summary>
    public string DestinationId { get; init; } = string.Empty;
    public string Scope { get; init; } = string.Empty;
    /// <summary>EMPTY on the seal row — that grant carries no timestamp on the wire (nests.md:67).</summary>
    public string Since { get; init; } = string.Empty;
    public string Status { get; init; } = string.Empty;
    public string BoundNote { get; init; } = string.Empty;
    public string RevokeLabel { get; init; } = string.Empty;
}

/// <summary>
/// One retained backup generation (<c>nest-trust-generation-item</c>) on the
/// home nest's row — a version a destination is still holding inside the
/// custody grace window. OneTime x:Bind-projected from
/// <c>TrustGenerationRow</c>; the restore address (<see cref="DestinationId"/>/
/// <see cref="FolderName"/>/<see cref="PathHash"/>/<see cref="ManifestHash"/>)
/// round-trips unchanged into <c>RestoreGeneration_Click</c> — never a row
/// index, which would promote the wrong version the moment this flattened
/// list is filtered or re-ordered (nests.md § Trust facet — generation
/// recovery). The whole item is bound onto the restore button's <c>Tag</c>
/// (<c>Tag="{x:Bind}"</c>), the same shape the sibling backup facet uses.
/// </summary>
public sealed class TrustGenerationItem
{
    // internal: TrustGenerationRow's own fields aren't UniFFI-internal types
    // (they're string/long), so these COULD be public, but they carry no
    // display meaning of their own — only the restore dispatch reads them —
    // so they stay internal for the same "XAML never touches it" reason as
    // TrustBackupItem.Kind above.
    internal string DestinationId { get; init; } = string.Empty;
    internal string FolderName { get; init; } = string.Empty;
    internal string PathHash { get; init; } = string.Empty;
    internal string ManifestHash { get; init; } = string.Empty;

    /// <summary>On an Unreachable row, the destination that went dark (no
    /// generation to identify); on a Listed row with no plaintext path,
    /// the path_hash instead — NEVER hidden or skipped (nests.md:123).</summary>
    public string PathText { get; init; } = string.Empty;
    /// <summary>EMPTY on an Unreachable row.</summary>
    public string Superseded { get; init; } = string.Empty;
    /// <summary>REQUIRED quota-bound copy. EMPTY on an Unreachable row.</summary>
    public string Expires { get; init; } = string.Empty;
    /// <summary>EMPTY on an Unreachable row.</summary>
    public string Size { get; init; } = string.Empty;
    public string Status { get; init; } = string.Empty;
    /// <summary>Collapsed when status=Unreachable — offering restore would
    /// imply a restore address we do not have (nests.md:122).</summary>
    public Visibility RestoreVisibility { get; init; } = Visibility.Collapsed;
    public string RestoreLabel { get; init; } = string.Empty;
}

/// <summary>
/// One nest row (<c>nests-item</c>), projected from <c>LinkedNestRow</c> for the
/// DataTemplate: the identity line (label / nest-id / sync capabilities / expiry
/// / unlink — the latter three hidden for the home row, which is the user's own
/// connected nest, not a pairing) plus the trust facet (per-row Now/History lens,
/// the grant list or <c>nest-trust-empty</c>, and the history list). OneTime
/// x:Bind targets — the list is rebuilt from the snapshot on every render.
/// </summary>
public sealed class NestRowItem
{
    /// <summary>Full hex nest id — carried on the lens-toggle / unlink buttons' Tag.</summary>
    public string NestIdFull { get; init; } = string.Empty;
    public bool IsHome { get; init; }
    public string Label { get; init; } = string.Empty;
    public string NestId { get; init; } = string.Empty;
    public string Capabilities { get; init; } = string.Empty;
    public string Expiry { get; init; } = string.Empty;
    public string UnlinkLabel { get; init; } = string.Empty;
    /// <summary>Sync-capabilities/expiry/unlink line — hidden for the home row.</summary>
    public Visibility IdentityDetailVisibility { get; init; } = Visibility.Visible;

    public string ViewNowLabel { get; init; } = string.Empty;
    public string ViewHistoryLabel { get; init; } = string.Empty;
    public Visibility NowLensVisibility { get; init; } = Visibility.Visible;
    public Visibility HistoryLensVisibility { get; init; } = Visibility.Collapsed;
    /// <summary>Now lens + grants present.</summary>
    public Visibility GrantListVisibility { get; init; } = Visibility.Collapsed;
    /// <summary>Now lens + no grants AND no backup rows — the "not trusted to
    /// read anything" state. A home nest holding backup rows is plainly
    /// trusted with something even with zero content grants, so this must
    /// also require zero <see cref="BackupItems"/> (nests.md:130).</summary>
    public Visibility EmptyVisibility { get; init; } = Visibility.Collapsed;
    /// <summary>"Not trusted to read anything" — named distinctly from the
    /// panel-level "No linked nests yet" x:Name="EmptyText" to avoid a
    /// WMC1507 named-element/field collision inside the DataTemplate.</summary>
    public string TrustEmptyText { get; init; } = string.Empty;
    public IReadOnlyList<TrustGrantItem> GrantItems { get; init; } = Array.Empty<TrustGrantItem>();
    /// <summary>Now lens + backup rows present (home row only — the shared
    /// machine populates <c>trustBackups</c> nowhere else). Independent of
    /// <see cref="GrantListVisibility"/>: rendered whenever backups exist,
    /// alongside either arm above, never itself gated on grants.Count.</summary>
    public Visibility BackupListVisibility { get; init; } = Visibility.Collapsed;
    public IReadOnlyList<TrustBackupItem> BackupItems { get; init; } = Array.Empty<TrustBackupItem>();
    /// <summary>Now lens + retained generations present (home row only —
    /// the shared machine populates <c>trustGenerations</c> nowhere else).
    /// Independent of grants/backups — a retained generation isn't a trust
    /// grant, so its presence alone never suppresses <c>nest-trust-empty</c>.</summary>
    public Visibility GenerationListVisibility { get; init; } = Visibility.Collapsed;
    public IReadOnlyList<TrustGenerationItem> GenerationItems { get; init; } = Array.Empty<TrustGenerationItem>();
    /// <summary>Home-row-scoped, NOT per-row: a restore's outcome describes the
    /// page's last action. Visible whenever the Now lens renders on the home
    /// row, regardless of <see cref="GenerationItems"/> count.</summary>
    public Visibility GenerationNoticeVisibility { get; init; } = Visibility.Collapsed;
    public string GenerationNoticeText { get; init; } = string.Empty;
    public IReadOnlyList<string> HistoryLines { get; init; } = Array.Empty<string>();

    /// <summary>Mint flow (nests.md § Mint, scope-first design ratified 2026-07-13):
    /// Now lens + a non-empty shared option catalog. The flow's own controls are
    /// built in code-behind (<see cref="NestsPanel.BuildMintFlow"/>) — genuinely
    /// dynamic per-row state (scope-driven holder visibility + confirm-enable) that
    /// OneTime x:Bind can't express, mirroring FoldersPage's dynamic-row idiom.</summary>
    public Visibility MintFlowVisibility { get; init; } = Visibility.Collapsed;
    // internal, not public: TrustMintOption is a UniFFI-internal type (CS0053 on a
    // public member — reference_windows_bindgen…/the FeedPostItemTests precedent).
    // Only code-behind (BuildMintFlow) reads it; XAML binds MintFlowVisibility only.
    internal TrustMintOption[] MintOptions { get; init; } = Array.Empty<TrustMintOption>();
}

/// <summary>
/// User-facing "Nests" surface (docs/goal/ui/nests.md): per-user nest pairing
/// (multi-homing, linked-nests.md — unchanged) plus the v1 nest-trust facet —
/// what content-processing each nest has been trusted to read, with a per-row
/// Now/History lens over the client-authoritative signed grant-event log,
/// per-grant renew/revoke, and the required honest bound, plus the v1 scope-first
/// mint flow (nests.md § Mint, ratified 2026-07-13) for authorizing a NEW
/// use-case. Dumb renderer of the shared <c>fauna_client_pair::LinkedNestsMachine</c>
/// (exposed over UniFFI by libs/fauna-ffi/src/pairing.rs) — no pairing or trust
/// logic in this shell (priority #2). Lifts the linux + web leads (apps/fauna-linux/src/settings/
/// linked_nests.rs; apps/fauna-web/src/lib/components/NestsSection.svelte):
/// hosted by its dedicated Settings shell sub-page <c>SettingsLinkedNestsPage</c>
/// (the nav slug/tag is <c>nests</c> fleet-wide, per nests.md, like
/// the testids/i18n/rail-label), which supplies
/// <c>ServiceClients</c> via <c>OnNavigatedTo</c> and surfaces this panel's
/// <see cref="ErrorChanged"/> on its own page-level <c>error-message</c>. Builds
/// the machine over the session's shared, auto-reconnecting WS-RPC connection
/// (the INestRpcClient seam), with both the mail relay-provisioning post-link
/// hook and the trust facet.
/// </summary>
public sealed partial class NestsPanel : UserControl
{
    private ServiceClients? _clients;
    private LinkedNestsMachine? _machine;
    private readonly ObservableCollection<NestRowItem> _nests = new();

    /// <summary>Raised with the machine's error message (or null to clear) so the
    /// host page can surface it through its own page-level <c>error-message</c> element.</summary>
    public event Action<string?>? ErrorChanged;

    public NestsPanel()
    {
        this.InitializeComponent();
        NestsList.ItemsSource = _nests;
        // Pairing a nest genuinely needs the nest (W4 (account-data-plane.md § Workstreams) phase 4). Revealing the form
        // (`AddButton`) and dismissing it (`CancelButton`) are pure local UI and
        // declare nothing — greying them would strand the user in a form they
        // cannot leave, which is the over-claim rulings 1-3 exist to prevent.
        SubmitButton.FaunaGate("fauna.pair.add");
    }

    /// <summary>Supplied by the host page (SettingsLinkedNestsPage.OnNavigatedTo) before the
    /// control's Loaded fires — carries the secrets used to build the WS client.</summary>
    internal void Configure(ServiceClients clients)
    {
        _clients = clients;
    }

    private async void Panel_Loaded(object sender, RoutedEventArgs e)
    {
        await LoadAsync();
    }

    private async Task EnsureMachineAsync()
    {
        if (_machine is not null || _clients?.Rpc is null) return;
        // Build over the session's shared, auto-reconnecting WS-RPC connection (the
        // INestRpcClient seam) rather than a per-panel one-shot FfiNestClient.Connect()
        // that would surface a transient os-error-10061 as a panel error while
        // shared-client pages recover. WITH both the mail relay-provisioning
        // post-link hook (a both-ends LinkBoth auto-provisions the just-linked
        // home box's mailbox reusing the fleet MSEK — deployment-home-with-public
        // -relay.md § Pairing) AND the trust facet (nests.md § Where logic lives);
        // the seam falls back to the plain machine if keypair derivation throws
        // (list/link/unlink still work; the hook + facet are then skipped).
        // Mirrors the linux lead (apps/fauna-linux/src/settings/linked_nests.rs).
        _machine = await _clients.Rpc.BuildLinkedNestsMachineWithMailRelayAndTrustAsync();
    }

    private async Task LoadAsync()
    {
        if (_clients is null) return;
        LoadingRing.IsActive = true;
        LoadingRing.Visibility = Visibility.Visible;
        try
        {
            await EnsureMachineAsync();
            // The transport already tolerates the post-login connect race for a
            // single RPC (transport.md § Request lifecycle step 3) — no app-level
            // retry needed here.
            await _machine!.Hydrate();
            RenderSnapshot(_machine!.Snapshot());
        }
        catch (Exception ex)
        {
            ErrorChanged?.Invoke(Strings.Error(ex));
        }
        finally
        {
            LoadingRing.IsActive = false;
            LoadingRing.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Dispatch an action, then render the resulting snapshot. The machine
    /// captures any user-facing error into <c>snapshot.error</c> (and also throws),
    /// so the throw is swallowed and the error read from the snapshot — matching the
    /// linux lead, which discards the dispatch Result and reads the snapshot.</summary>
    private async Task DispatchAsync(LinkedNestsAction action)
    {
        if (_machine is null) return;
        try
        {
            await _machine.Dispatch(action);
        }
        catch (Exception)
        {
            // Error surfaced via snapshot.error below.
        }
        RenderSnapshot(_machine.Snapshot());
    }

    private void RenderSnapshot(LinkedNestsSnapshot snap)
    {
        // A mutation in flight disables the add button so it can't double-fire.
        AddButton.IsEnabled = snap.status != LinkedNestStatus.Working;

        ErrorChanged?.Invoke(string.IsNullOrEmpty(snap.error) ? null : snap.error);

        // The home nest row (nests.md § Layout — the connected nest) renders
        // first, then the pairings.
        _nests.Clear();
        // restoreOutcome is snapshot-level (the page's LAST restore action, not
        // any one row's), so it is threaded into ToItem only for the home row —
        // mirrors linux's `if row.is_home` gate on nest-trust-generation-notice.
        if (snap.home is { } home) _nests.Add(ToItem(home, snap.restoreOutcome));
        foreach (var row in snap.pairings)
        {
            _nests.Add(ToItem(row, null));
        }
        EmptyText.Visibility = _nests.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private static NestRowItem ToItem(LinkedNestRow row, TrustRestoreOutcome? restoreOutcome)
    {
        var abbreviated = Abbreviate(row.nestId);
        var label = string.IsNullOrEmpty(row.label) ? abbreviated : row.label!;
        var caps = string.Join(", ", row.capabilities);
        var expiry = row.expiresAt.HasValue
            ? S.Get("nests/expiry_label")
            : S.Get("nests/expiry_never");

        var isNowLens = row.lens == TrustLens.Now;
        var grants = row.trustGrants.Select(ToGrantItem).ToList();
        var backups = row.trustBackups.Select(ToBackupItem).ToList();
        var generations = row.trustGenerations.Select(ToGenerationItem).ToList();
        var historyLines = row.trustHistory.Select(NestTrustFormat.HistoryLine).ToList();

        return new NestRowItem
        {
            NestIdFull = row.nestId,
            IsHome = row.isHome,
            Label = label,
            NestId = abbreviated,
            Capabilities = caps,
            Expiry = expiry,
            UnlinkLabel = S.Get("nests/unlink"),
            IdentityDetailVisibility = row.isHome ? Visibility.Collapsed : Visibility.Visible,
            ViewNowLabel = S.Get("nests/view_now"),
            ViewHistoryLabel = S.Get("nests/view_history"),
            NowLensVisibility = isNowLens ? Visibility.Visible : Visibility.Collapsed,
            HistoryLensVisibility = isNowLens ? Visibility.Collapsed : Visibility.Visible,
            GrantListVisibility = isNowLens && grants.Count > 0 ? Visibility.Visible : Visibility.Collapsed,
            EmptyVisibility = isNowLens && grants.Count == 0 && backups.Count == 0
                ? Visibility.Visible : Visibility.Collapsed,
            TrustEmptyText = S.Get("nests/not_trusted"),
            GrantItems = grants,
            BackupListVisibility = isNowLens && backups.Count > 0 ? Visibility.Visible : Visibility.Collapsed,
            BackupItems = backups,
            // NOT part of EmptyVisibility above: a retained generation isn't a
            // trust grant, so its presence alone doesn't mean "trusted with
            // something" (linux's row.trust_grants.is_empty() && row.trust_backups
            // .is_empty() gate omits generations for the same reason).
            GenerationListVisibility = isNowLens && generations.Count > 0
                ? Visibility.Visible : Visibility.Collapsed,
            GenerationItems = generations,
            // Home-row-scoped, NOT per-row (nests.md § Trust facet — generation
            // recovery) — registered whenever the Now lens renders on the HOME
            // row, empty until a restore resolves.
            GenerationNoticeVisibility = isNowLens && row.isHome
                ? Visibility.Visible : Visibility.Collapsed,
            GenerationNoticeText = restoreOutcome switch
            {
                TrustRestoreOutcome.Restored => S.Get("nests/generation_restored"),
                TrustRestoreOutcome.PastRecoveryWindow => S.Get("nests/generation_past_window"),
                null => string.Empty,
                _ => string.Empty,
            },
            HistoryLines = historyLines,
            // Only when the shared option catalog is non-empty — an empty catalog
            // means nothing derivable or no discoverable holder, never a picker
            // that can only error (mirrors linux's build_mint_flow gate).
            MintFlowVisibility = isNowLens && row.mintOptions.Length > 0
                ? Visibility.Visible : Visibility.Collapsed,
            MintOptions = row.mintOptions,
        };
    }

    private static TrustGrantItem ToGrantItem(TrustGrantRow g) => new()
    {
        GrantId = g.grantId,
        Scope = $"{S.Get("nests/trusted_to_read")} {NestTrustFormat.ScopeLine(g.scope)}",
        LastsUntil = $"{S.Get("nests/lasts_until")} {FaunaFfiMethods.FormatUnixLocal(g.lastsUntil)}",
        Status = NestTrustFormat.StatusLabel(g.liveness),
        // REQUIRED honest-bound copy (nests.md § Honest bound) — never
        // over-promise. A bounded (content-sealing-epochs) mail grant gets the
        // stronger, crypto-bounded wording WITH the honest INFO-A caveat; every
        // other kind/regime keeps the standing trust-until-revoke wording
        // (flip-checklist line 6). Never re-derive the (class, kind, tier) check
        // here — the shared predicate is the single source of truth (priority #2).
        BoundNote = S.Get(
            FaunaClientPairMethods.TrustScopeIsBoundedMailGrant(g.scope)
                ? "nests/bound_note_bounded_mail"
                : "nests/bound_note_standing"),
        RenewLabel = S.Get("nests/renew"),
        RevokeLabel = S.Get("nests/revoke"),
        UnattestedVisibility = g.unattested ? Visibility.Visible : Visibility.Collapsed,
        UnattestedMark = S.Get("nests/grant_unattested_mark"),
        KeepLabel = S.Get("nests/grant_keep_button"),
    };

    private static TrustBackupItem ToBackupItem(TrustBackupRow b) => new()
    {
        Kind = b.kind,
        DestinationId = b.destinationId,
        Scope = b.kind == TrustBackupKind.Seal
            ? S.Get("nests/backup_scope_seal")
            : S.Format("nests/backup_scope_writer", b.destinationLabel),
        // EMPTY on the seal row — that grant carries no timestamp on the wire
        // (nests.md:67); the leaf still exists so the row's leaf set doesn't
        // vary by kind.
        Since = b.since is { } at
            ? $"{S.Get("nests/backup_since")} {FaunaFfiMethods.FormatUnixLocal(at)}"
            : string.Empty,
        // Shared Rust owns the status vocabulary (priority #2) — never a
        // hand-rolled switch here, matching linux/web.
        Status = S.Resolve(FaunaClientPairMethods.BackupStatusLabel(b.status)),
        // REQUIRED honest-bound copy (nests.md § Honest bound) — revoking
        // freezes only FUTURE writes; already-held custody remains until the
        // holder reclaims it. Never over-promise.
        BoundNote = S.Get(b.kind == TrustBackupKind.Seal
            ? "nests/backup_bound_note_seal"
            : "nests/backup_bound_note_writer"),
        RevokeLabel = S.Get("nests/backup_revoke"),
    };

    private static TrustGenerationItem ToGenerationItem(TrustGenerationRow g)
    {
        var unreachable = g.status == TrustGenerationStatus.Unreachable;
        return new TrustGenerationItem
        {
            DestinationId = g.destinationId,
            FolderName = g.folderName,
            PathHash = g.pathHash,
            ManifestHash = g.manifestHash,
            // On an Unreachable row this names the DESTINATION that went dark
            // (no generation to identify); on a Listed row with no plaintext
            // path (a sealed custody row with its path scrubbed), the
            // path_hash instead — NEVER hidden or skipped (nests.md:123).
            PathText = unreachable
                ? g.destinationLabel
                : g.path is { } path
                    ? S.Format("nests/generation_path", path)
                    : S.Format("nests/generation_path_unknown", g.pathHash),
            Superseded = unreachable
                ? string.Empty
                : $"{S.Get("nests/generation_superseded")} {FaunaFfiMethods.FormatUnixLocal(g.supersededAt)}",
            // REQUIRED quota-bound copy (nests.md § Required copy — the quota
            // bound): retained versions are charged for the WHOLE window.
            Expires = unreachable
                ? string.Empty
                : S.Format("nests/generation_expires", FaunaFfiMethods.FormatUnixLocal(g.expiresAt)),
            Size = unreachable ? string.Empty : ValueFormat.ByteSize((ulong)g.sizeBytes),
            // "Could not reach" is deliberately NOT the same as "nothing to
            // restore" — collapsing them is the false reassurance this whole
            // surface exists to prevent (nests.md:122). No shared Rust status
            // door exists for this one (unlike backup status) — matches
            // linux's own hand-rolled S::GENERATION_STATUS_* constants.
            Status = S.Get(unreachable ? "nests/generation_status_unreachable" : "nests/generation_status_listed"),
            // Absent when unreachable — offering restore would imply an
            // address we do not have.
            RestoreVisibility = unreachable ? Visibility.Collapsed : Visibility.Visible,
            RestoreLabel = S.Get("nests/generation_restore"),
        };
    }

    /// <summary>Short display form of a 64-hex nest identity — the shared
    /// <c>short_id</c> (first 12 chars + <c>…</c>), so windows renders nest-ids
    /// identically to web/iOS/Android/linux instead of a first-8…last-8 form
    /// (docs/goal/behavior/value-formatting.md § Short id; priority #1/#4).</summary>
    private static string Abbreviate(string nestId) => FaunaFfiMethods.ShortId(nestId);

    private void AddButton_Click(object sender, RoutedEventArgs e)
    {
        AddInput.Text = string.Empty;
        AddForm.Visibility = Visibility.Visible;
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        AddForm.Visibility = Visibility.Collapsed;
    }

    private async void Submit_Click(object sender, RoutedEventArgs e)
    {
        var raw = AddInput.Text.Trim();
        // Route the entered value through the shared classifier so every app
        // resolves the same input identically (priority #2): a nest address (URL)
        // seeds the authorization row on BOTH ends in one action (LinkBoth — the
        // common "log into 2 nests, then pair them" case), while a bare 64-hex
        // Ed25519 identity authorizes that one nest (Link, the out-of-band
        // single-end path). Mirrors linux submit_link
        // (apps/fauna-linux/src/settings/linked_nests.rs). Empty capabilities →
        // the machine fills the canonical full self-sync set.
        LinkedNestsAction action = FaunaFfiMethods.ClassifyLinkInput(raw) switch
        {
            LinkInput.NestUrl url => new LinkedNestsAction.LinkBoth(
                url.nestUrl, Array.Empty<string>(), null, null),
            LinkInput.NestId id => new LinkedNestsAction.Link(
                id.nestId, Array.Empty<string>(), null, null, null),
            var other => throw new InvalidOperationException(
                $"unhandled LinkInput variant {other.GetType().Name}"),
        };
        await DispatchAsync(action);
        // Close the form only on success (no error came back).
        if (_machine is not null && string.IsNullOrEmpty(_machine.Snapshot().error))
        {
            AddForm.Visibility = Visibility.Collapsed;
        }
    }

    private async void Unlink_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn && btn.Tag is string nestId)
        {
            await DispatchAsync(new LinkedNestsAction.Unlink(nestId));
        }
    }

    private async void ViewNow_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn && btn.Tag is string nestId)
        {
            await DispatchAsync(new LinkedNestsAction.SetLens(nestId, TrustLens.Now));
        }
    }

    private async void ViewHistory_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn && btn.Tag is string nestId)
        {
            await DispatchAsync(new LinkedNestsAction.SetLens(nestId, TrustLens.History));
        }
    }

    private async void Renew_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn && btn.Tag is byte[] grantId)
        {
            await DispatchAsync(new LinkedNestsAction.Renew(grantId));
        }
    }

    private async void Revoke_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn && btn.Tag is byte[] grantId)
        {
            await DispatchAsync(new LinkedNestsAction.Revoke(grantId));
        }
    }

    /// <summary><c>nest-trust-grant-keep-button</c> — the Keep half of Keep/Revoke
    /// on a grant the succession aftermath carried across. The shared machine
    /// clears the mark at rest (config-only, no nest round trip, no grant event);
    /// the re-rendered snapshot drops the pair because the row now reads
    /// <c>unattested: false</c>. No confirm: Keep is non-destructive and
    /// re-decidable.</summary>
    private async void KeepGrant_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn && btn.Tag is byte[] grantId)
        {
            await DispatchAsync(new LinkedNestsAction.KeepGrant(grantId));
        }
    }

    /// <summary>The <c>nest-trust-backup-revoke</c> press on a backup row. The
    /// shared machine routes it: a seal row's revoke to the source nest
    /// (<c>fauna.backup.nest_key.revoke</c>), a writer row's revoke to
    /// <b>the destination</b> over its own connection
    /// (<c>fauna.backup.writer_grant.revoke</c>) — this shell only names the
    /// row and dispatches; it holds no trust logic (priority #2).</summary>
    private async void RevokeBackup_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: TrustBackupItem item }) return;
        LinkedNestsAction action = item.Kind == TrustBackupKind.Seal
            ? new LinkedNestsAction.RevokeBackupSeal()
            : new LinkedNestsAction.RevokeBackupWriter(item.DestinationId);
        await DispatchAsync(action);
    }

    /// <summary>The <c>nest-trust-generation-restore</c> press on a generation
    /// row. The address TRIPLE (plus destination) round-trips off the row
    /// unchanged — never a row index, which would promote the wrong version
    /// the moment this flattened list is filtered or re-ordered. The resulting
    /// re-render's <c>restoreOutcome</c> settles onto
    /// <c>nest-trust-generation-notice</c> (never <c>error-message</c> —
    /// <c>PastRecoveryWindow</c> is a product state, not a failure).</summary>
    private async void RestoreGeneration_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: TrustGenerationItem item }) return;
        await DispatchAsync(new LinkedNestsAction.RestoreGeneration(
            item.DestinationId, item.FolderName, item.PathHash, item.ManifestHash));
    }

    // ── Mint flow (nests.md § Mint) ─────────────────────────────────
    //
    // Built in code-behind, not XAML/x:Bind — the scope-select's SelectionChanged
    // drives two genuinely dynamic sibling states (the confirm button's enabled
    // state + the holder select's visibility) that OneTime x:Bind can't express
    // without a full per-row ViewModel. Mirrors FoldersPage's BuildConflictPolicyRow/
    // BuildPaywallTierRow idiom: local closures capture the row's own controls
    // directly, no cross-control dictionary bookkeeping needed. Placeholder host
    // bridges the XAML DataTemplate into this code-behind builder, keyed off the
    // container's auto-set DataContext (the AdminUsersPage `{ DataContext: Row }`
    // pattern this app already uses for templated-row dynamic controls).

    private void MintHost_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is not StackPanel { DataContext: NestRowItem row } host) return;
        // A non-virtualizing ItemsPanel keeps containers stable, but guard anyway
        // (cheap) so a Loaded refire never double-builds the flow.
        if (host.Children.Count > 0) return;
        host.Children.Add(BuildMintFlow(row));
    }

    /// <summary>
    /// Build one row's mint flow: <c>nest-trust-grant-mint-button</c> (reveals the
    /// form) → <c>nest-trust-mint-scope-select</c> (option 0 = placeholder, option
    /// i+1 = <c>row.MintOptions[i]</c>) → conditional <c>nest-trust-mint-holder-select</c>
    /// (shown only when the chosen option lists &gt;1 candidate) →
    /// <c>nest-trust-mint-confirm-button</c> (enabled once a real option is chosen).
    /// Confirm dispatches <c>Mint{nest_id, holder_bridge_id, scope}</c>; the resulting
    /// re-render rebuilds this row from scratch, collapsing the form. Mirrors linux
    /// <c>build_mint_flow</c> step for step (apps/fauna-linux/src/settings/linked_nests.rs).
    /// </summary>
    private FrameworkElement BuildMintFlow(NestRowItem row)
    {
        var mintBox = new StackPanel { Spacing = 4, Margin = new Thickness(0, 4, 0, 0) };

        var mintBtn = new Button
        {
            Content = S.Get("nests/mint_button"),
            HorizontalAlignment = HorizontalAlignment.Left,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(mintBtn, Ids.NestTrustGrantMintButton);
        mintBox.Children.Add(mintBtn);

        // The form, revealed by the mint button.
        var form = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4, Visibility = Visibility.Collapsed };

        var scopeCombo = new ComboBox { MinWidth = 220 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(scopeCombo, Ids.NestTrustMintScopeSelect);
        scopeCombo.Items.Add(new ComboBoxItem { Content = S.Get("nests/mint_scope_placeholder") });
        foreach (var opt in row.MintOptions)
        {
            scopeCombo.Items.Add(new ComboBoxItem { Content = NestTrustFormat.MintOptionLabel(opt) });
        }
        scopeCombo.SelectedIndex = 0;
        form.Children.Add(scopeCombo);

        // Conditional holder select — populated + shown only when the chosen option
        // has >1 candidate. Candidates are the stable holder names (bridge_id, e.g.
        // "web-serve"); a localized holder-label catalog arrives with the first
        // deployment that actually hits this arm (matches the linux comment).
        var holderCombo = new ComboBox { MinWidth = 160, Visibility = Visibility.Collapsed };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(holderCombo, Ids.NestTrustMintHolderSelect);
        form.Children.Add(holderCombo);

        var confirmBtn = new Button
        {
            Content = S.Get("nests/mint_confirm"),
            Style = (Style)Application.Current.Resources["AccentButtonStyle"],
            IsEnabled = false,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(confirmBtn, Ids.NestTrustMintConfirmButton);
        form.Children.Add(confirmBtn);

        mintBox.Children.Add(form);

        mintBtn.Click += (_, _) => { form.Visibility = Visibility.Visible; };

        scopeCombo.SelectionChanged += (_, _) =>
        {
            var idx = scopeCombo.SelectedIndex;
            var picked = idx > 0 ? row.MintOptions[idx - 1] : null;
            confirmBtn.IsEnabled = picked is not null;
            if (picked is { } o && o.holderCandidates.Length > 1)
            {
                holderCombo.Items.Clear();
                foreach (var h in o.holderCandidates)
                {
                    holderCombo.Items.Add(new ComboBoxItem { Content = h });
                }
                holderCombo.SelectedIndex = 0;
                holderCombo.Visibility = Visibility.Visible;
            }
            else
            {
                holderCombo.Visibility = Visibility.Collapsed;
            }
        };

        confirmBtn.Click += async (_, _) =>
        {
            var idx = scopeCombo.SelectedIndex;
            if (idx <= 0) return;
            var option = row.MintOptions[idx - 1];
            // Derived holder: the single candidate; ambiguity → the holder select's
            // pick (visible iff >1 candidate).
            string holderBridgeId;
            if (option.holderCandidates.Length > 1)
            {
                var h = holderCombo.SelectedIndex;
                if (h < 0) return;
                holderBridgeId = option.holderCandidates[h];
            }
            else
            {
                holderBridgeId = option.holderCandidates[0];
            }
            // The standard window until the duration picker + blessing toggle lift
            // here (nests.md § Expiry / renewal → Duration and blessing).
            await DispatchAsync(new LinkedNestsAction.Mint(row.NestIdFull, holderBridgeId, option.scope, TrustGrantDuration.Standard));
        };

        return mintBox;
    }
}
