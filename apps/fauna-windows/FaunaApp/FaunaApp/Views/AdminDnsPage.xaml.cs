using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using Fauna.Generated;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_client_dns;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_client_pair;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// Admin page: unified DNS, manual-mode read + live red/green verify
/// (`admin-dns`). Dumb renderer of the shared <c>DnsManagementMachine</c>
/// (libs/fauna-client-dns, exposed over UniFFI by libs/fauna-ffi/src/mail_admin.rs):
/// builds the machine over the shared, already-connected WS-RPC client (the
/// <c>INestRpcClient</c> seam), hydrates (= Refresh /
/// list_records), overlays live verdicts (VerifyRecords / verify_records), and
/// renders one section per domain with its required-record matrix plus a
/// per-domain Fauna-managed / manual mode control. Behavior:
/// docs/goal/behavior/dns-management.md § The two modes + § Manual + live
/// verification. IDs: tests/e2e-unified/ui.yaml. Prior art:
/// apps/fauna-linux/src/views/admin.rs (build_dns_page / mode ToggleButton) +
/// apps/fauna-linux/src/client.rs (dns_set_mode).
/// </summary>
public sealed partial class AdminDnsPage : Page
{
    private ServiceClients? _clients;
    private DnsManagementMachine? _machine;
    private LocalDomainMachine? _localDomains;
    private readonly ObservableCollection<DnsDomainItem> _domains = new();
    private readonly ObservableCollection<DnsCredentialItem> _credentials = new();
    private readonly ObservableCollection<DnsRemovedDomainItem> _removedDomains = new();

    /// <summary>True once the local-domains CRUD list has hydrated. Until then (or
    /// after a list-load failure) <see cref="RenderAll"/> falls back to rendering the
    /// DNS snapshot's domains read-only — mirrors linux <c>render_admin_dns</c>'s
    /// "snapshot not in yet" arm so the record matrix appears even if the CRUD list
    /// is slow / unavailable.</summary>
    private bool _ldHydrated;

    /// <summary>A local-domains list-hydrate failure message, kept so RenderAll can
    /// surface it without blanking the independently-loaded DNS record matrix.</summary>
    private string? _ldLoadError;

    /// <summary>The current snapshot's <c>LocalDomainsSnapshot.addingFirstDomain</c> —
    /// a client-side hint mirroring the nest's own <c>is_primary</c> derivation
    /// ("no active rows yet"), never re-derived here. Cached from <see cref="RenderAll"/>
    /// so <see cref="AddDomain_Click"/> can gate <c>AddDomainPrimaryWarning</c>
    /// (`mail-multidomain.md` § Adding a new local domain step 7) without a
    /// second snapshot read.</summary>
    private bool _addingFirstDomain;

    /// <summary>Add-domain wizard default — the new-domain MTA-STS cert mode pick
    /// (mail-multidomain.md § Adding a new local domain step 3). Mirrors the
    /// shared-Rust <c>local_domains.rs</c> <c>DEFAULT_CERT_MODE</c> (not exposed as a
    /// const over UniFFI). The slice-1 add form is name-only, so this is the applied
    /// default. The MTA-STS policy mode is not sent: the nest sets and advances
    /// it.</summary>
    private const string DefaultCertMode = "expand_primary";

    /// <summary>Guards the programmatic <c>ToggleSwitch.IsOn</c> set during a
    /// reflective render of <c>ManageAllToggle</c> so its <c>Toggled</c> handler
    /// doesn't re-dispatch (matches linux's <c>dns_manage_all_guard</c> cell).</summary>
    private bool _syncingManageAll;

    /// <summary>The add-credential form's selected provider id, plus the live
    /// (field_id, input-box) pairs the submit reads — rebuilt on provider-select
    /// (mirrors linux <c>build_add_credential_form</c>'s <c>selected</c> +
    /// <c>entries</c>).</summary>
    private string? _selectedCredProvider;
    private readonly List<(string Id, Control Input)> _credFieldEntries = new();

    /// <summary>Every account on the nest, offered by the per-domain catch-all AND
    /// role-address pickers (admin.md § 2 → <i>Which accounts a picker offers</i>).
    /// Loaded once per page-load alongside the machines; empty until
    /// <see cref="LoadAdminActorsAsync"/> returns (the picker then offers only
    /// "None").</summary>
    private List<(byte[] Id, string Label)> _adminActors = new();

    public AdminDnsPage()
    {
        this.InitializeComponent();
        DomainsList.ItemsSource = _domains;
        CredentialsList.ItemsSource = _credentials;
        RemovedDomainsList.ItemsSource = _removedDomains;
        BuildCredentialProviderRow();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        await LoadAsync();
    }

    private async Task LoadAsync()
    {
        if (_clients is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await EnsureMachineAsync();
            // DNS record matrix (list_records) — the expected per-record values.
            await _machine!.Hydrate();
            // The authoritative domain-CRUD list (active + soft-deleted). Caught
            // separately so a list-load failure doesn't blank the record matrix —
            // RenderAll then falls back to the DNS snapshot's domains read-only.
            _ldHydrated = false;
            _ldLoadError = null;
            try
            {
                await _localDomains!.Hydrate();
                _ldHydrated = true;
            }
            catch (Exception lx)
            {
                _ldLoadError = Strings.Error(lx);
            }
            // Deployment actors for the per-domain catch-all picker. Tolerated
            // separately (like the cert-status arm) so an admin-users RPC hiccup
            // leaves the picker offering only "None" rather than blanking the page.
            await LoadAdminActorsAsync();
            RenderAll();
            // Served-cert health (`fauna.tls.cert_status`) → snapshot.cert_statuses,
            // rendered as the per-domain admin-dns-cert-status badge (Slice 1,
            // tls-certificates.md § C.4). A pure Admin read; tolerated separately so
            // a cert-status RPC hiccup leaves the badge "checking" rather than
            // blanking the record matrix. Mirrors linux's RefreshCertStatus dispatch.
            try
            {
                await _machine.Dispatch(new DnsAction.RefreshCertStatus());
            }
            catch (Exception cx)
            {
                ShellLog.Warn("AdminDnsPage", $"cert-status refresh failed: {cx.Message}");
            }
            RenderAll();
            // Overlay the live red/green verify verdicts — same cycle linux runs.
            await _machine.Dispatch(new DnsAction.VerifyRecords(null));
            RenderAll();
            // The nest mints each domain's DKIM key itself
            // (mail-bridge-lifecycle.md § DKIM provisioning); no client-side sweep.
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    private async Task EnsureMachineAsync()
    {
        if (_machine is not null || _clients?.Rpc is null) return;
        // Build both machines over the shared, already-connected, auto-reconnecting
        // WS-RPC client (the INestRpcClient seam) rather than a one-shot per-page
        // FfiNestClient.Connect() — a transient WS blip then recovers instead of
        // pinning the page in an os-error-10061.
        //
        // Credential-loading variant (not the read/verify-only BuildDnsManagementMachine):
        // the per-domain mode toggle dispatches SetMode, which needs the
        // tip-sealed fauna.state.dns credential record, read through the account runtime, and the actor
        // keypair (the seam supplies the secret from the session crypto). Without a
        // covering credential opt-in still rejects with InvalidState (surfaced in
        // error-message). Mirrors linux dns_set_mode.
        _machine = await _clients.Rpc.BuildDnsManagementMachineWithCredentialsAsync();
        // The shared LocalDomainMachine drives the domain-CRUD half (add/remove/
        // restore over fauna.bridges.{add,remove,restore}_local_domain). Its
        // LocalDomainsSnapshot is merged onto the same page by domain name (mirrors
        // linux render_admin_dns merging LocalDomainsSnapshot + DnsSnapshot).
        _localDomains = await _clients.Rpc.BuildLocalDomainsMachineAsync();
    }

    /// <summary>Load every account on the nest for the per-domain catch-all AND
    /// role-address pickers (<c>fauna_client_admin::users_list_all</c>; admin.md § 2 →
    /// <i>Which accounts a picker offers</i>). A failure is logged and swallowed (the
    /// pickers then offer only their clear option); no <c>ConfigureAwait(false)</c>
    /// (off-thread bound-state mutation throws a silent COMException).</summary>
    private async Task LoadAdminActorsAsync()
    {
        if (_clients?.Rpc is null) return;
        try
        {
            var users = await _clients.Rpc.AdminUsersListAllAsync();
            _adminActors = users
                .Select(u => (Id: u.actorId, Label: AdminActorOptions.Label(u)))
                .ToList();
        }
        catch (Exception ax)
        {
            ShellLog.Warn("AdminDnsPage",
                $"admin-users load for catch-all picker failed: {ax.Message}");
        }
    }

    /// <summary>Re-render the whole page from both machines' current snapshots: the
    /// active domain sections (the authoritative <c>LocalDomainsSnapshot.active</c>
    /// rows merged with the <c>DnsSnapshot</c> record matrix by name), the
    /// soft-deleted rows, the held credentials, and the manage-all master switch.
    /// Error precedence mirrors linux <c>render_admin_dns</c>: the local-domains CRUD
    /// feedback (e.g. <c>cannot_remove_primary_domain</c>) wins, then a list-load
    /// failure, then the DNS list/verify/credential error.</summary>
    private void RenderAll()
    {
        var dns = _machine!.Snapshot();
        var ld = _ldHydrated ? _localDomains!.Snapshot() : null;
        _addingFirstDomain = ld?.addingFirstDomain ?? false;
        RenderDomains(dns, ld);
        RenderRenameBanner(ld);
        RenderRemovedDomains(ld);
        RenderCredentials(dns);
        RenderManageAll(dns, ld);
        var err = ld?.error ?? _ldLoadError ?? dns.error;
        if (err is { } e)
        {
            ShowError(e);
        }
        else
        {
            ClearError();
        }
    }

    /// <summary>Rebuild the held DNS-provider credential list (the managed-mode
    /// "Fauna controls DNS" store) from <c>DnsSnapshot.credentials</c> — provider
    /// id + covered zones + the snapshot index that keys its <c>ClearCredentials</c>.
    /// Never renders secrets (the add-form is write-only). Mirrors linux
    /// <c>build_credential_item</c>; the empty caption shows when none are held.</summary>
    private void RenderCredentials(DnsSnapshot snap)
    {
        _credentials.Clear();
        var zonesCaption = S.Get("admin/dns/credential_zones");
        var clearLabel = S.Get("admin/dns/remove");
        uint i = 0;
        foreach (var c in snap.credentials)
        {
            // "Zones: a.test, b.test" (or the bare caption when verify reported
            // none) — the e2e reads this and asserts the covered zone is present.
            var zones = c.zones.Any()
                ? $"{zonesCaption}: {string.Join(", ", c.zones)}"
                : zonesCaption;
            _credentials.Add(new DnsCredentialItem
            {
                Index = i,
                ProviderText = c.providerId,
                ZonesText = zones,
                ClearLabel = clearLabel,
            });
            i++;
        }
        CredentialsEmpty.Visibility =
            _credentials.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>Reflectively sync the deployment "manage all domains" master switch
    /// from the same all-managed projection linux/web use (every active domain in
    /// <c>managed</c> mode), guarding the programmatic <c>IsOn</c> set so its
    /// <c>Toggled</c> handler doesn't re-dispatch. This is the sole DNS
    /// master-switch render after the admin-services page was removed (2026-06-04,
    /// admin.md § Admin IA redesign).</summary>
    private void RenderManageAll(DnsSnapshot dns, LocalDomainsSnapshot? ld)
    {
        // Active-domain set: prefer the authoritative local-domains active list, fall
        // back to the DNS snapshot's own domains until it arrives (mirrors linux).
        // The reflected on/off state is the shared projection
        // (DnsManagementMachine.all_domains_managed, libs/fauna-client-dns) — the
        // same single source of truth linux/web use — not a re-coded
        // `mode == "managed"` fold.
        var active = (ld is not null
            ? ld.active.Select(d => d.domain)
            : dns.domains.Select(d => d.domain)).ToArray();
        var allManaged = _machine is not null && _machine.AllDomainsManaged(active);
        _syncingManageAll = true;
        ManageAllToggle.IsOn = allManaged;
        _syncingManageAll = false;
    }

    /// <summary>Rebuild the active per-domain rows by merging the authoritative
    /// <c>LocalDomainsSnapshot.active</c> list (carries <c>is_primary</c>) with the
    /// <c>DnsSnapshot</c> record matrix by domain name (mirrors linux
    /// <c>render_admin_dns</c>). Until the local-domains list is in (or if it failed
    /// to load) <paramref name="ld"/> is null and we fall back to the DNS domains
    /// read-only so records still appear. Doesn't touch the error bar, so the
    /// mode-toggle handler can re-render rows while keeping a just-surfaced rejection
    /// in <c>error-message</c>.</summary>
    private void RenderDomains(DnsSnapshot dns, LocalDomainsSnapshot? ld)
    {
        _domains.Clear();
        var ctx = new DomainRenderContext(dns);

        if (ld is not null)
        {
            // The local-domains active list is authoritative for the CRUD list
            // (and is_primary); overlay each row's DNS record matrix by name. The
            // primary-domain-rename affordances read the snapshot-wide rename_available
            // gate + the single active_rename projection (same for every row).
            foreach (var d in ld.active)
            {
                ctx.DnsByName.TryGetValue(d.domain, out var view);
                AddDomainItem(d.domain, d.isPrimary, view, d.catchAllActorId,
                    d.catchAllClearedBySuccessionAt,
                    d.roleAddressOverrides, true, ld.renameAvailable, ld.activeRename, ctx);
            }
        }
        else
        {
            // Local-domains snapshot not in yet (or failed) — render the DNS domains
            // read-only so records appear; is_primary unknown (remove enabled, but the
            // nest still refuses the primary). The catch-all designation is unknown
            // here too, so the picker is collapsed (catchAllKnown: false). The rename
            // affordances need the authoritative CRUD snapshot, so they stay hidden on
            // the read-only fallback row (renameAvailable: false, activeRename: null).
            // Mirrors linux render_admin_dns's None arm.
            foreach (var view in dns.domains)
            {
                AddDomainItem(view.domain, false, view, null, null, null, false, false, null, ctx);
            }
        }

        EmptyText.Visibility = _domains.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>Per-render lookups + captions shared by every domain section,
    /// computed once from the DNS snapshot (mirrors linux <c>render_admin_dns</c>'s
    /// pre-loop maps): the record matrix / served-cert / CNAME-delegation lookups by
    /// domain name, the single in-flight manual-paste order, the held credentials'
    /// covered zones (the delegate-zone-select options), and the shared role-vocabulary
    /// table.</summary>
    private sealed class DomainRenderContext
    {
        public readonly Dictionary<string, DomainView> DnsByName = new();
        public readonly Dictionary<string, CertStatusRow> CertByName = new();
        public readonly Dictionary<string, DelegationView> DelegationByName = new();
        public readonly PendingCertIssue? Pending;
        public readonly IReadOnlyList<string> CredZones;
        /// <summary>The shared role vocabulary
        /// (<c>fauna_client_mail_settings::local_domains::role_address_options()</c>) —
        /// every per-domain role picker looks its role up in this table by key; nothing
        /// re-derives the map by switching on the enum (mail-multidomain.md § Per-domain
        /// role-address routing → *One owner for the role vocabulary*).</summary>
        public readonly IReadOnlyList<RoleAddressOption> RoleOptions =
            FaunaClientMailSettingsMethods.RoleAddressOptions();

        // Captions (resolved once per render, not per domain).
        public readonly string NameCaption = S.Get("admin/dns/field_name");
        public readonly string TypeCaption = S.Get("admin/dns/field_type");
        public readonly string ValueCaption = S.Get("admin/dns/field_value");
        public readonly string CopyLabel = S.Get("admin/dns/copy");
        public readonly string ModeManaged = S.Get("admin/dns/mode_managed");
        public readonly string ModeManual = S.Get("admin/dns/mode_manual");
        public readonly string PrimaryBadge = S.Get("admin/dns/primary_badge");
        public readonly string RemoveLabel = S.Get("admin/dns/remove");
        public readonly string RenameLabel = S.Get("admin/dns/rename/button");
        public readonly string PromoteLabel = S.Get("admin/dns/rename/promote");
        public readonly string RenamingToLabel = S.Get("admin/dns/rename/renaming_to");
        public readonly string CatchAllLabel = S.Get("admin/dns/catch_all_label");
        public readonly string CatchAllNone = S.Get("admin/dns/catch_all_none");
        public readonly string CatchAllClearedBySuccession = S.Get("admin/dns/catch_all_cleared_by_succession");
        public readonly string RoleAddressLabel = S.Get("admin/dns/role_address_label");
        public readonly string RoleAddressAdminDefault = S.Get("admin/dns/role_address_admin_default");
        public readonly string AutoRenewLabel = S.Get("admin/dns/cert/auto_renew");
        public readonly string IssueLabel = S.Get("admin/dns/cert/issue");
        public readonly string CompleteLabel = S.Get("admin/dns/cert/issue_complete");
        public readonly string CancelLabel = S.Get("admin/dns/cert/issue_cancel");
        public readonly string PasteInstructions = S.Get("admin/dns/cert/paste_instructions");
        public readonly string DelegateLabel = S.Get("admin/dns/cert/delegate");
        public readonly string DelegateZoneLabel = S.Get("admin/dns/cert/delegate_zone_label");
        public readonly string DelegateSubmitLabel = S.Get("admin/dns/cert/delegate_submit");
        public readonly string DelegateCancelLabel = S.Get("admin/dns/cert/delegate_cancel");
        public readonly string RemoveDelegationLabel = S.Get("admin/dns/cert/remove_delegation");
        public readonly string RenewalsAutomatedLabel = S.Get("admin/dns/cert/renewals_automated");
        public readonly string DelegateNoZonesLabel = S.Get("admin/dns/cert/delegate_no_zones");

        public DomainRenderContext(DnsSnapshot dns)
        {
            foreach (var dv in dns.domains) DnsByName[dv.domain] = dv;
            foreach (var c in dns.certStatuses) CertByName[c.domain] = c;
            foreach (var d in dns.delegations) DelegationByName[d.domain] = d;
            Pending = dns.pendingCert;
            // Zones the held credentials cover — the delegate-zone-select options
            // (sorted + deduped; empty disables the delegate affordance).
            var zones = dns.credentials.SelectMany(c => c.zones).Distinct().ToList();
            zones.Sort(StringComparer.Ordinal);
            CredZones = zones;
        }
    }

    /// <summary>Build one active-domain section (header + record rows) given its
    /// optional DNS record-matrix view. Effective mode is the shared machine's
    /// <c>DomainView::is_managed</c> projection (the button label IS that mode text —
    /// the cross-app e2e contract — so it snaps back to "Manual" on a rejected
    /// opt-in); "manual" when no matrix view exists yet. The primary row shows the
    /// read-only badge and a disabled remove-button. <paramref name="renameAvailable"/>
    /// + <paramref name="activeRename"/> are the snapshot-wide primary-domain-rename
    /// gate + projection (same for every row) driving the per-row rename/promote/state
    /// affordances.</summary>
    private void AddDomainItem(string domain, bool isPrimary, DomainView? view,
        byte[]? catchAllActorId, long? catchAllClearedBySuccessionAt,
        IReadOnlyList<RoleAddressOverrideView>? roleOverrides,
        bool catchAllKnown, bool renameAvailable, PrimaryDomainRenameView? activeRename,
        DomainRenderContext ctx)
    {
        var records = new List<DnsRecordItem>();
        if (view is not null)
        {
            foreach (var r in view.records)
            {
                records.Add(BuildRecordItem(r, ctx));
            }
        }
        var isManaged = view is not null && FaunaClientDnsMethods.DomainIsManaged(view);
        ctx.CertByName.TryGetValue(domain, out var cert);
        ctx.DelegationByName.TryGetValue(domain, out var delegation);
        var isDelegated = delegation is not null;
        // A managed or CNAME-delegated domain auto-issues with a single dispatch
        // (IssueCert); a manual, undelegated domain opens the two-phase manual paste
        // flow (BeginManualIssueCert). Mirrors linux `single_issue`.
        var isSingleIssue = isManaged || isDelegated;
        // One manual order at a time: the paste surface (and the disabled issue
        // button) render only for the single domain the suspended order is for.
        var pendingHere = ctx.Pending is not null && ctx.Pending.domain == domain;

        // Auto-renew (tls-certificates.md § C.3): shown ONLY for managed/delegated
        // rows — the only kind a synced client can auto-issue — default-on.
        var autoRenewVisible = isManaged || isDelegated;
        var autoRenewOn = view is not null && view.autoRenew;

        var (certText, certBrush) = CertStatusDisplay(cert);

        // The transient `_acme-challenge` TXT(s) to paste — the same admin-dns-record
        // card every record uses (one record type, one path). Present only while a
        // manual order awaits this domain's admin.
        var pendingChallenges = new List<DnsRecordItem>();
        if (pendingHere && ctx.Pending is not null)
        {
            foreach (var ch in ctx.Pending.challenges)
            {
                pendingChallenges.Add(BuildRecordItem(ch, ctx));
            }
        }
        // The one-time CNAME the admin sets once (rendered as an admin-dns-record).
        var delegationCname = new List<DnsRecordItem>();
        if (delegation is not null)
        {
            delegationCname.Add(BuildRecordItem(delegation.cname, ctx));
        }

        // Per-domain catch-all picker options (admin-dns-domain-catch-all-select).
        // Index 0 = "None" (clears); index i>0 = an actor; the parallel CatchAllActorIds
        // carries the actor id (null at 0) the SelectionChanged handler dispatches. A
        // designation not among the loaded actors (paginated out) gets a trailing
        // "actor xxxx…" entry keeping it visible + selected. Built by the shared
        // BuildActorPicker — the same option/index shape the role-address pickers below
        // use (mirrors linux build_domain_section).
        var (caOptions, caIds, caSelected) = BuildActorPicker(catchAllActorId, ctx.CatchAllNone);

        // Per-domain role-address override pickers (admin-dns-domain-role-address-<role>-select).
        // One actor dropdown per overridable role (postmaster/abuse/noc/security); index 0
        // = "Admin (default)" clears (the role falls back to the deployment admin). The
        // current designation per role is read off the projected role_address_overrides (a
        // role absent from the list has no override). Same option/index shape as catch-all,
        // ×4 — mirrors linux's `for role in RoleAddressKind::ALL` loop. Each named slot is
        // looked up in the shared table by key (fail-closed — RoleOption throws on a
        // missing/renamed key rather than silently mis-routing), never by switching on the
        // enum.
        var postmasterPicker = BuildRolePicker(RoleOption(ctx.RoleOptions, "postmaster"), roleOverrides, ctx);
        var abusePicker = BuildRolePicker(RoleOption(ctx.RoleOptions, "abuse"), roleOverrides, ctx);
        var nocPicker = BuildRolePicker(RoleOption(ctx.RoleOptions, "noc"), roleOverrides, ctx);
        var securityPicker = BuildRolePicker(RoleOption(ctx.RoleOptions, "security"), roleOverrides, ctx);

        // Primary-domain rename affordances (mail-primary-domain-rename.md § UX surface;
        // mirrors web +page.svelte row block + linux build_domain_section). PRIMARY row
        // → rename-button (enabled iff a non-primary domain exists to promote AND no
        // rename is in flight) + the in-flight state badge; NON-PRIMARY row → the
        // promote shortcut, hidden while a rename runs.
        var renameActive = activeRename is not null;
        var renameStateText = renameActive
            ? $"{ctx.RenamingToLabel} {activeRename!.newPrimaryDomain} ({activeRename.state})"
            : "";

        _domains.Add(new DnsDomainItem
        {
            DomainName = domain,
            IsManaged = isManaged,
            ModeLabel = isManaged ? ctx.ModeManaged : ctx.ModeManual,
            IsPrimary = isPrimary,
            PrimaryBadgeText = ctx.PrimaryBadge,
            PrimaryBadgeVisibility = isPrimary ? Visibility.Visible : Visibility.Collapsed,
            RemoveLabel = ctx.RemoveLabel,
            RemoveEnabled = !isPrimary,

            // Primary-domain rename affordances.
            RenameButtonLabel = ctx.RenameLabel,
            RenameButtonVisibility = isPrimary ? Visibility.Visible : Visibility.Collapsed,
            RenameEnabled = renameAvailable && !renameActive,
            PromoteButtonLabel = ctx.PromoteLabel,
            // Gated on catchAllKnown (the authoritative local-domains row is in) so the
            // read-only DNS-fallback rows — which pass isPrimary: false for every row —
            // don't sprout a promote button with no snapshot behind it (the primary
            // row's rename-button is already isPrimary-gated, so the fallback never
            // shows it).
            PromoteButtonVisibility = (catchAllKnown && !isPrimary && !renameActive)
                ? Visibility.Visible : Visibility.Collapsed,
            RenameStateVisibility = (isPrimary && renameActive)
                ? Visibility.Visible : Visibility.Collapsed,
            RenameStateText = renameStateText,

            Records = records,

            // Slice 1 — served-cert health badge.
            CertStatusText = certText,
            CertStatusBrush = certBrush,

            // Slice 3 — issuance + manual paste.
            IssueLabel = ctx.IssueLabel,
            IssueEnabled = !pendingHere,
            IsSingleIssue = isSingleIssue,
            PendingVisibility = pendingHere ? Visibility.Visible : Visibility.Collapsed,
            PasteInstructions = ctx.PasteInstructions,
            PendingChallenges = pendingChallenges,
            CompleteLabel = ctx.CompleteLabel,
            CancelLabel = ctx.CancelLabel,

            // Slice 2 — CNAME renewal-delegation.
            IsDelegated = isDelegated,
            DelegatedVisibility = isDelegated ? Visibility.Visible : Visibility.Collapsed,
            UndelegatedVisibility = isDelegated ? Visibility.Collapsed : Visibility.Visible,
            RenewalsAutomatedLabel = ctx.RenewalsAutomatedLabel,
            RemoveDelegationLabel = ctx.RemoveDelegationLabel,
            DelegationCname = delegationCname,
            DelegateLabel = ctx.DelegateLabel,
            DelegateEnabled = ctx.CredZones.Count > 0,
            DelegateNoZonesLabel = ctx.CredZones.Count > 0 ? null : ctx.DelegateNoZonesLabel,
            DelegateZoneLabel = ctx.DelegateZoneLabel,
            DelegateZones = ctx.CredZones.ToList(),
            DelegateSubmitLabel = ctx.DelegateSubmitLabel,
            DelegateCancelLabel = ctx.DelegateCancelLabel,

            // Slice 4a — auto-renew checkbox.
            AutoRenewLabel = ctx.AutoRenewLabel,
            AutoRenewVisibility = autoRenewVisible ? Visibility.Visible : Visibility.Collapsed,
            AutoRenewOn = autoRenewOn,
            AutoRenewState = autoRenewOn ? "on" : "off",

            // Per-domain catch-all picker (admin-dns-domain-catch-all-select).
            CatchAllLabel = ctx.CatchAllLabel,
            CatchAllOptions = caOptions,
            CatchAllActorIds = caIds,
            CatchAllSelectedIndex = caSelected,
            CatchAllVisibility = catchAllKnown ? Visibility.Visible : Visibility.Collapsed,
            CatchAllClearedText = ctx.CatchAllClearedBySuccession,
            CatchAllClearedVisibility = catchAllClearedBySuccessionAt.HasValue
                ? Visibility.Visible : Visibility.Collapsed,

            // Per-domain role-address pickers (admin-dns-domain-role-address-<role>-select).
            // Gated on the same authoritative-row knowledge as catch-all.
            RoleAddressLabel = ctx.RoleAddressLabel,
            RoleAddressVisibility = catchAllKnown ? Visibility.Visible : Visibility.Collapsed,
            PostmasterPicker = postmasterPicker,
            AbusePicker = abusePicker,
            NocPicker = nocPicker,
            SecurityPicker = securityPicker,
        });
    }

    /// <summary>Build a per-domain actor picker's options + parallel actor-id map +
    /// current selection — the shared shape both the catch-all and the four
    /// role-address pickers use. Index 0 = <paramref name="clearLabel"/> ("None" /
    /// "Admin (default)") → null actor id (clears); index i>0 = <c>_adminActors[i-1]</c>.
    /// A current designation not among the loaded actors (paginated out) gets a trailing
    /// "actor xxxx…" entry keeping it visible + selected rather than silently clearing it.
    /// Mirrors linux build_domain_section's label/id/selected build.</summary>
    private (List<string> Options, List<byte[]?> ActorIds, int SelectedIndex) BuildActorPicker(
        byte[]? currentActorId, string clearLabel)
    {
        var options = new List<string> { clearLabel };
        var ids = new List<byte[]?> { null };
        foreach (var (id, label) in _adminActors)
        {
            options.Add(label);
            ids.Add(id);
        }
        var selected = 0;
        if (currentActorId is { } cur)
        {
            var found = _adminActors.FindIndex(a => a.Id.AsSpan().SequenceEqual(cur));
            if (found >= 0)
            {
                selected = found + 1;
            }
            else
            {
                options.Add(AdminActorOptions.NotLoadedFallbackLabel(cur));
                ids.Add(cur);
                selected = options.Count - 1;
            }
        }
        return (options, ids, selected);
    }

    /// <summary>Look up one named role's entry in the shared
    /// <see cref="FaunaClientMailSettingsMethods.RoleAddressOptions"/> table by its storage
    /// key. Fail-closed: an unmatched key throws rather than silently resolving to some
    /// other role (the hazard the hand-rolled map's <c>_ =&gt; ""</c> /
    /// <c>_ =&gt; RoleAddressKind.Postmaster</c> arms had — mail-multidomain.md § Per-domain
    /// role-address routing → *One owner for the role vocabulary*).</summary>
    private static RoleAddressOption RoleOption(IReadOnlyList<RoleAddressOption> table, string key) =>
        table.FirstOrDefault(o => o.key == key)
        ?? throw new InvalidOperationException($"role_address_options() is missing the '{key}' entry");

    /// <summary>Build one role's picker item (`admin-dns-domain-role-address-&lt;role&gt;-select`).
    /// The current designation is the actor for this role in the projected
    /// <c>role_address_overrides</c> (a role absent from the list has no override → falls
    /// back to the admin, so index 0 / "Admin (default)"). FFI/wasm consumers iterate the
    /// override list themselves (the Rust <c>role_override</c> helper isn't exported);
    /// option 0's label is "Admin (default)" (clearing reverts to the admin, not "no
    /// routing"). Mirrors linux build_domain_section's per-role loop body.</summary>
    private RoleAddressPickerItem BuildRolePicker(
        RoleAddressOption option, IReadOnlyList<RoleAddressOverrideView>? roleOverrides,
        DomainRenderContext ctx)
    {
        // Project the actor for this role out of the override list (a role absent from
        // the list has no override). Select-then-FirstOrDefault so the lookup is correct
        // whether the UniFFI record projects as a class or a struct.
        byte[]? current = roleOverrides?
            .Where(o => o.role == option.kind)
            .Select(o => o.actorId)
            .FirstOrDefault();
        var (options, ids, selected) = BuildActorPicker(current, ctx.RoleAddressAdminDefault);
        return new RoleAddressPickerItem
        {
            RoleKey = option.key,
            Kind = option.kind,
            RoleLabel = $"{option.key}@",
            Options = options,
            ActorIds = ids,
            SelectedIndex = selected,
        };
    }

    /// <summary>Build one <see cref="DnsRecordItem"/> from a shared
    /// <c>DnsRecordRow</c> — the <c>admin-dns-record</c> card the record matrix, the
    /// manual-paste challenges, and the CNAME-delegation row all reuse (one record
    /// type, one path). A missing verdict reads as "checking" (neutral).</summary>
    private static DnsRecordItem BuildRecordItem(DnsRecordRow r, DomainRenderContext ctx)
    {
        var (statusText, statusBrush) = VerdictDisplay(r.verdict);
        var isPtr = r.recordType == "PTR";
        return new DnsRecordItem
        {
            NameCaption = ctx.NameCaption,
            Name = r.name,
            TypeCaption = ctx.TypeCaption,
            RecordType = r.recordType,
            ValueCaption = ctx.ValueCaption,
            Value = r.expected,
            StatusText = statusText,
            StatusBrush = statusBrush,
            CopyLabel = ctx.CopyLabel,
            ProviderNote = isPtr ? S.Get("admin/dns/ptr_provider_note") : "",
            ProviderNoteVisibility = isPtr ? Visibility.Visible : Visibility.Collapsed,
        };
    }

    /// <summary>Map the served-cert health row to the badge text + brush
    /// (`admin-dns-cert-status`, tls-certificates.md § C.4). Mirrors linux
    /// <c>cert_status_text</c>/<c>cert_status_css</c>: null (not yet refreshed) →
    /// "checking" (neutral); ValidTrusted → valid (success); OnFloorRenewNeeded →
    /// renew-needed (warning) + " (self-signed)"; Expiring → expiring (warning) +
    /// the expiry date. A trusted cert appends its notAfter date.</summary>
    private static (string, Brush?) CertStatusDisplay(CertStatusRow? cert)
    {
        var label = S.Get("admin/dns/cert/label");
        if (cert is null)
        {
            return ($"{label} {S.Get("admin/dns/status_checking")}", ThemeBrush("TextFillColorTertiaryBrush"));
        }
        var view = FaunaFfiMethods.CertStatusView(cert.state.ToString(), cert.isFloor, cert.notAfterUnix);
        var text = $"{label} {Strings.Resolve(view.state)}";
        if (view.showSelfSigned)
        {
            // On the self-signed floor — a trusted cert is needed; the floor's own
            // expiry is not the admin's concern, so we label it self-signed.
            text += $" ({S.Get("admin/dns/cert/self_signed")})";
        }
        else if (view.expiresAtUnix is { } expiresAt)
        {
            // A trusted cert (valid or expiring) — surface its expiry date. The
            // windows resw is flat, so substitute the {date} placeholder in C#.
            var date = FormatCertExpiry(expiresAt);
            text += " — " + S.Get("admin/dns/cert/expires").Replace("{date}", date);
        }
        // Colour is deliberately NOT part of the shared view (idiomatic per-app
        // render, same split as contact_status_label) — keep deriving it from
        // cert.state locally.
        var brushKey = cert.state switch
        {
            CertHealthState.ValidTrusted => "SystemFillColorSuccessBrush",
            CertHealthState.OnFloorRenewNeeded => "SystemFillColorCautionBrush",
            _ => "SystemFillColorCautionBrush",
        };
        return (text, ThemeBrush(brushKey));
    }

    /// <summary>Format a cert notAfter (unix seconds) as the shared local YYYY-MM-DD
    /// date for the cert-status badge; renders the raw seconds out of range, never
    /// throws. Same shared fn linux consumes (value-formatting.md § Absolute local
    /// timestamp display → the date-only sibling).</summary>
    private static string FormatCertExpiry(long unixSecs) =>
        FaunaFfiMethods.FormatUnixLocalDate(unixSecs);

    /// <summary>Rebuild the soft-deleted (recently-removed) domain rows from the
    /// local-domains snapshot — each a name + Restore button (30-day recovery,
    /// mail-multidomain.md § Re-add within 30 days). The section hides when none are
    /// removed (so the <c>admin-dns-removed-domain-name</c> count is 0).</summary>
    private void RenderRemovedDomains(LocalDomainsSnapshot? ld)
    {
        _removedDomains.Clear();
        var restoreLabel = S.Get("admin/dns/restore");
        if (ld is not null)
        {
            foreach (var d in ld.softDeleted)
            {
                _removedDomains.Add(new DnsRemovedDomainItem
                {
                    DomainName = d.domain,
                    RestoreLabel = restoreLabel,
                });
            }
        }
        RemovedSection.Visibility =
            _removedDomains.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>
    /// Flip a domain's Fauna-managed / manual mode (the per-domain
    /// <c>admin-dns-domain-mode</c> control). Mirrors linux
    /// <c>FaunaClient::dns_set_mode</c>: Hydrate (load matrix + held credential
    /// store + project effective modes) → <c>SetMode</c>. Opting **in** requires a
    /// held DNS-provider credential whose zones cover the domain
    /// (dns-management.md § The two modes); without one the shared
    /// <c>DnsManagementMachine</c>'s SetMode rejects (InvalidState), the rejection
    /// lands in the snapshot's <c>error</c> (surfaced in <c>error-message</c>), and
    /// the domain stays manual. A successful opt-in publishes the record matrix
    /// inside the same SetMode (the shared machine owns that sequencing — there is
    /// no manual Publish button, dns-management.md § Fauna-managed), and a publish
    /// failure surfaces but does **not** fall back to manual. After a clean
    /// SetMode we re-verify to refresh the red/green verdicts. A throw
    /// skips the remaining dispatches, so a VerifyRecords never wipes a rejection
    /// before it renders.
    /// </summary>
    private async void DomainMode_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not DnsDomainItem item) return;
        if (_machine is null) return;

        var managed = !item.IsManaged;
        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        string? caughtError = null;
        try
        {
            await _machine.Hydrate();
            await _machine.Dispatch(new DnsAction.SetMode(item.DomainName, managed));
            await _machine.Dispatch(new DnsAction.VerifyRecords(null));
        }
        catch (Exception ex)
        {
            caughtError = Strings.Error(ex);
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }

        // Re-render the rows so the button label snaps back to the achieved
        // effective mode. Prefer the machine's stored rejection over the caught
        // exception — they describe the same failure and the snapshot message is
        // the user-facing one (matches linux, which renders snapshot.error). This is
        // a DNS-mode operation, so the relevant error is the DNS snapshot's.
        var dns = _machine.Snapshot();
        var ld = _ldHydrated ? _localDomains!.Snapshot() : null;
        RenderDomains(dns, ld);
        RenderManageAll(dns, ld);
        if (dns.error is { } err) ShowError(err);
        else if (caughtError is not null) ShowError(caughtError);
        else ClearError();
    }

    // ── Cert lifecycle (issuance / manual paste / delegation / auto-renew) ──────

    /// <summary>Get/renew a domain's TLS cert (`admin-dns-cert-issue-button`). A
    /// managed or CNAME-delegated domain fires a single client-driven DNS-01 order
    /// (`IssueCert`); a manual, undelegated domain opens the two-phase manual-paste
    /// flow (`BeginManualIssueCert` → the `_acme-challenge` paste surface +
    /// complete/cancel). `target_nest_id` is the connected nest's identity
    /// (`LinkedNestsMachine.this_nest().id`, uniform across all 7 apps — same
    /// source `NestsPanel` uses). Native-only; mirrors linux `dns_issue_cert` /
    /// `dns_begin_manual_issue`. The CA round-trip itself can't complete in a no-CA
    /// env (the order errors, surfaced in `error-message`); the pebble test covers
    /// the full round-trip.</summary>
    private async void CertIssue_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not DnsDomainItem item) return;
        if (_machine is null || _clients?.Rpc is null) return;
        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            // Resolve the home nest the cert serves (this_nest = the connected nest),
            // over the shared WS-RPC client (the INestRpcClient seam).
            var self = await (await _clients.Rpc.BuildLinkedNestsMachineAsync()).ThisNest();
            DnsAction action = item.IsSingleIssue
                ? new DnsAction.IssueCert(item.DomainName, self.id)
                : (DnsAction)new DnsAction.BeginManualIssueCert(item.DomainName, self.id);
            await _machine.Dispatch(action);
            await _machine.Dispatch(new DnsAction.RefreshCertStatus());
            RenderAll();
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Finalize a suspended manual order once the admin pasted the
    /// `_acme-challenge` TXT (`admin-dns-cert-complete-button` →
    /// `CompleteManualIssueCert`).</summary>
    private async void CertComplete_Click(object sender, RoutedEventArgs e)
        => await DispatchCertAsync(new DnsAction.CompleteManualIssueCert(), refreshStatus: true);

    /// <summary>Drop the suspended manual order — the nest stays on the floor
    /// (`admin-dns-cert-cancel-button` → `CancelManualIssueCert`).</summary>
    private async void CertCancel_Click(object sender, RoutedEventArgs e)
        => await DispatchCertAsync(new DnsAction.CancelManualIssueCert(), refreshStatus: false);

    /// <summary>Reveal a manual domain's CNAME renewal-delegation form
    /// (`admin-dns-cert-delegate-button`). A per-row visual toggle (the Tag-marked
    /// sibling form panel) — no re-render, so the suspended reveal survives until the
    /// admin submits or cancels. Mirrors linux's `set_visible` toggle.</summary>
    private void CertDelegate_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement btn || btn.Parent is not Panel parent) return;
        if (ChildByTag(parent, "delegate-form") is not { } form) return;
        form.Visibility = Visibility.Visible;
        btn.Visibility = Visibility.Collapsed;
    }

    /// <summary>Hide the delegate form, restore the reveal button
    /// (`admin-dns-cert-delegate-cancel-button`).</summary>
    private void CertDelegateCancel_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement cancel) return;
        if (cancel.Parent is not Panel form || form.Parent is not Panel outer) return;
        form.Visibility = Visibility.Collapsed;
        if (outer.Children.OfType<Button>().FirstOrDefault() is { } btn)
        {
            btn.Visibility = Visibility.Visible;
        }
    }

    /// <summary>Delegate the manual domain's `_acme-challenge` renewals to the
    /// selected held-credential zone (`admin-dns-cert-delegate-submit-button` →
    /// `DelegateRenewal{domain, target_zone}`, tls-certificates.md § B tier 3 S6b).
    /// Reads the row's zone-select; defaults to the first zone if none picked
    /// (matches the e2e, which delegates to the default first entry).</summary>
    private async void CertDelegateSubmit_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not DnsDomainItem item) return;
        if (sender is not FrameworkElement submit || submit.Parent is not Panel form) return;
        var combo = form.Children.OfType<ComboBox>().FirstOrDefault();
        var zone = combo?.SelectedItem as string ?? item.DelegateZones.FirstOrDefault();
        if (string.IsNullOrEmpty(zone)) return;
        await DispatchCertAsync(new DnsAction.DelegateRenewal(item.DomainName, zone), refreshStatus: false);
    }

    /// <summary>Remove a domain's CNAME renewal-delegation
    /// (`admin-dns-cert-remove-delegation-button` → `RemoveDelegation`), reverting it
    /// to the delegate affordance.</summary>
    private async void CertRemoveDelegation_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not DnsDomainItem item) return;
        await DispatchCertAsync(new DnsAction.RemoveDelegation(item.DomainName), refreshStatus: false);
    }

    /// <summary>Flip a managed/delegated domain's auto-renew opt-out
    /// (`admin-dns-domain-auto-renew` → `SetAutoRenew`, tls-certificates.md § C.3).
    /// FlaUI drives the CheckBox via the Toggle pattern (fires Checked/Unchecked, not
    /// Click), and the x:Bind sets IsChecked at render time — which would also raise
    /// the event — so we ignore any change that merely matches the model's current
    /// value (the render-time echo) and act only on a genuine user flip.</summary>
    private async void AutoRenew_Toggled(object sender, RoutedEventArgs e)
    {
        if (sender is not CheckBox cb || cb.DataContext is not DnsDomainItem item) return;
        var enabled = cb.IsChecked == true;
        if (enabled == item.AutoRenewOn) return; // render-time binding echo, not a user action
        if (_machine is null) return;
        await DispatchCertAsync(new DnsAction.SetAutoRenew(item.DomainName, enabled), refreshStatus: false);
    }

    /// <summary>Dispatch a cert action, optionally refresh the served-cert status,
    /// then re-render. A failure surfaces in `error-message`; the page stays
    /// rendered. No `ConfigureAwait(false)` (off-thread bound-state mutation throws a
    /// silent COMException).</summary>
    private async Task DispatchCertAsync(DnsAction action, bool refreshStatus)
    {
        if (_machine is null) return;
        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await _machine.Dispatch(action);
            if (refreshStatus)
            {
                await _machine.Dispatch(new DnsAction.RefreshCertStatus());
            }
            RenderAll();
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>First direct child of <paramref name="panel"/> whose <c>Tag</c>
    /// equals <paramref name="tag"/> — used to locate a per-row reveal form within
    /// its DataTemplate row without an x:Name (unavailable across template
    /// instances).</summary>
    private static FrameworkElement? ChildByTag(Panel panel, string tag)
        => panel.Children.OfType<FrameworkElement>().FirstOrDefault(c => (c.Tag as string) == tag);

    /// <summary>Map a record's optional verify verdict to display text + brush.
    /// A missing verdict reads as "checking" (neutral), never a false red/green.
    /// The observed values ride along so a mismatch says what public DNS actually
    /// served ("Mismatch - found 1.2.3.4") instead of dead-ending.</summary>
    private static (string, Brush?) VerdictDisplay(RecordVerdict? verdict)
    {
        var status = verdict?.status;
        var text = Strings.Resolve(FaunaFfiMethods.DnsVerdictLabel(
            status?.ToString() ?? "",
            verdict?.observed ?? Array.Empty<string>()));
        var brush = status switch
        {
            VerifyStatus.Ok => ThemeBrush("SystemFillColorSuccessBrush"),
            VerifyStatus.Missing => ThemeBrush("SystemFillColorCriticalBrush"),
            VerifyStatus.Mismatch => ThemeBrush("SystemFillColorCriticalBrush"),
            _ => ThemeBrush("TextFillColorTertiaryBrush"),
        };
        return (text, brush);
    }

    private static Brush? ThemeBrush(string key)
    {
        try
        {
            return Application.Current.Resources.TryGetValue(key, out var v) ? v as Brush : null;
        }
        catch
        {
            return null;
        }
    }

    private void CopyButton_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not DnsRecordItem item) return;
        try
        {
            FaunaApp.Helpers.ClipboardHelper.CopyText(item.Value);
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
    }

    private void ShowError(string msg)
    {
        ErrorBar.Message = msg;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = msg;
    }

    private void ClearError()
    {
        ErrorBar.IsOpen = false;
        App.CurrentErrorMessage = null;
    }

    // ── Refresh + manage-all (page controls) ────────────────────────────────

    /// <summary>Re-fetch the record matrix + held-credential store + live verdicts
    /// (`admin-dns-refresh-button`). Same cycle as the initial load — Hydrate
    /// (= list_records + credential reload) then VerifyRecords.</summary>
    private async void Refresh_Click(object sender, RoutedEventArgs e)
        => await LoadAsync();

    /// <summary>Flip the deployment "manage all domains" master switch
    /// (`admin-dns-manage-all-toggle`). Guarded against the reflective
    /// <c>IsOn</c> set in <see cref="RenderManageAll"/>.</summary>
    private async void ManageAll_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncingManageAll || _machine is null) return;
        await SetAllManagedAsync(ManageAllToggle.IsOn);
    }

    /// <summary>Set every active domain's mode (SetMode publishes on opt-in), then re-verify
    /// and re-render. Opting in needs a held credential covering each domain; the
    /// first rejection lands in the snapshot's <c>error</c> (surfaced in
    /// <c>error-message</c>) and the re-render snaps the toggle back to the achieved
    /// truth. Mirrors linux <c>dns_set_all_managed</c>.</summary>
    private async Task SetAllManagedAsync(bool managed)
    {
        if (_machine is null) return;
        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await _machine.Hydrate();
            var domains = _machine.Snapshot().domains.Select(d => d.domain).ToList();
            foreach (var domain in domains)
            {
                await _machine.Dispatch(new DnsAction.SetMode(domain, managed));
            }
            await _machine.Dispatch(new DnsAction.VerifyRecords(null));
            RenderAll();
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
            // Snap the toggle back to whatever the server reflects after the failure.
            try
            {
                RenderManageAll(_machine.Snapshot(),
                    _ldHydrated ? _localDomains!.Snapshot() : null);
            }
            catch (Exception reEx)
            {
                ShellLog.Warn("AdminDnsPage",
                    $"manage-all re-render failed: {reEx.Message}");
            }
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    // ── Held-credential list (clear) ────────────────────────────────────────

    /// <summary>Remove a held credential by its snapshot index
    /// (`admin-dns-credential-item-clear-button` → <c>ClearCredentials</c>).
    /// Domains that lose coverage re-render <c>"manual"</c> via the effective-mode
    /// projection.</summary>
    private async void ClearCredential_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not DnsCredentialItem item) return;
        if (_machine is null) return;
        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await _machine.Dispatch(new DnsAction.ClearCredentials(item.Index));
            RenderAll();
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    // ── Write-only add-credential form ──────────────────────────────────────

    /// <summary>Populate the provider-row with one button per DNS-capable provider
    /// (`admin-dns-add-credential-provider-row[&lt;pid&gt;]`, the same keyed idiom as
    /// onboarding's <c>dns-provider-row</c>). Selecting one rebuilds the field bag.
    /// Built once from the generated registry; mirrors
    /// <see cref="Onboarding.DnsConfigView.BuildDnsProviderRow"/>.</summary>
    private void BuildCredentialProviderRow()
    {
        AddCredentialProviderRow.Children.Clear();
        foreach (var p in Providers.All.Where(p => p.Capabilities.Contains(Capability.Dns)))
        {
            var label = ResolveLabel(p.DisplayNameKey);
            var btn = new Button { Content = label };
            btn.SetValue(AutomationProperties.AutomationIdProperty,
                $"admin-dns-add-credential-provider-row[{p.Id}]");
            var pid = p.Id;
            btn.Click += (_, _) => SelectCredentialProvider(pid);
            AddCredentialProviderRow.Children.Add(btn);
        }
    }

    private void SelectCredentialProvider(string providerId)
    {
        _selectedCredProvider = providerId;
        RebuildCredentialFields(providerId);
    }

    /// <summary>Rebuild the add-credential field bag for the selected provider —
    /// one TextBox/PasswordBox per <c>Capability.Dns</c> field, the input's
    /// AutomationId being the raw providers.yaml field id (e.g. <c>api-token</c>),
    /// so the e2e bridge types into it directly. Mirrors linux
    /// <c>rebuild_credential_fields</c> / <see cref="Controls.GenericProviderForm"/>.</summary>
    private void RebuildCredentialFields(string providerId)
    {
        AddCredentialFields.Children.Clear();
        _credFieldEntries.Clear();
        var provider = Providers.All.FirstOrDefault(p => p.Id == providerId);
        if (provider is null) return;
        foreach (var f in provider.Fields.Where(f => f.Kinds.Contains(Capability.Dns)))
        {
            var label = ResolveLabel(f.LabelKey);
            Control input = f.Type == FieldType.Secret || f.Type == FieldType.HostedAuth
                ? new PasswordBox { Header = label }
                : new TextBox { Header = label };
            input.SetValue(AutomationProperties.AutomationIdProperty, f.Id);
            AddCredentialFields.Children.Add(input);
            _credFieldEntries.Add((f.Id, input));
        }
    }

    /// <summary>Resolve a provisioning label/name key (dotted in the registry,
    /// slashed in resw) to its localized string, falling back to the bare key on
    /// miss. Mirrors <see cref="Controls.GenericProviderForm.ResolveLabel"/>.</summary>
    private static string ResolveLabel(string key)
    {
        var slashed = key.Replace('.', '/');
        var resolved = S.Get(slashed);
        return resolved == slashed ? key : resolved;
    }

    private static string ReadField(Control input) => input switch
    {
        PasswordBox pb => pb.Password,
        TextBox tb => tb.Text,
        _ => "",
    };

    /// <summary>Reveal the write-only add-credential form (`admin-dns-add-credential-button`).</summary>
    private void AddCredential_Click(object sender, RoutedEventArgs e)
    {
        AddCredentialButton.Visibility = Visibility.Collapsed;
        AddCredentialForm.Visibility = Visibility.Visible;
    }

    private void AddCredentialCancel_Click(object sender, RoutedEventArgs e)
        => ResetCredentialForm();

    private void ResetCredentialForm()
    {
        _selectedCredProvider = null;
        _credFieldEntries.Clear();
        AddCredentialFields.Children.Clear();
        AddCredentialForm.Visibility = Visibility.Collapsed;
        AddCredentialButton.Visibility = Visibility.Visible;
    }

    /// <summary>Collect the typed (field_id, value) bag and dispatch
    /// <c>PutCredentials</c> (verify against the provider API, then seal into
    /// <c>fauna.state.dns</c>). No provider selected / any field blank → no-op (keep
    /// the form open; a blank token would fail provider verify anyway). On success
    /// the credential renders in the held list; a failed verify surfaces in
    /// <c>error-message</c> and stores nothing. The credential label defaults to the
    /// provider id (mirrors linux <c>dns_put_credentials</c>).</summary>
    private async void AddCredentialSubmit_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null) return;
        var pid = _selectedCredProvider;
        if (pid is null) return;
        var fields = _credFieldEntries
            .Select(en => new DnsCredentialField(en.Id, ReadField(en.Input)))
            .ToArray();
        if (fields.Length == 0 || fields.Any(f => string.IsNullOrWhiteSpace(f.value)))
        {
            return;
        }

        ResetCredentialForm();
        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await _machine.Dispatch(new DnsAction.PutCredentials(pid, fields, pid));
            RenderAll();
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    // ── Domain CRUD (add / remove / restore over the shared LocalDomainMachine) ──

    /// <summary>Reveal the inline add-domain form (`admin-dns-add-domain-button`).</summary>
    private void AddDomain_Click(object sender, RoutedEventArgs e)
    {
        AddDomainButton.Visibility = Visibility.Collapsed;
        AddDomainForm.Visibility = Visibility.Visible;
        // Row 365: the add-first-domain confirm's irreversibility warning
        // (mail-multidomain.md § Adding a new local domain step 7). Untagged
        // chrome, revealed together with the form and gated on the same
        // client-side hint tui/linux/web/android/apple already consume — never
        // re-derived here.
        AddDomainPrimaryWarning.Visibility =
            _addingFirstDomain ? Visibility.Visible : Visibility.Collapsed;
        AddDomainInput.Focus(FocusState.Programmatic);
    }

    /// <summary>Clear + hide the add-domain form (`admin-dns-add-domain-cancel-button`).</summary>
    private void AddDomainCancel_Click(object sender, RoutedEventArgs e)
        => ResetAddDomainForm();

    private void ResetAddDomainForm()
    {
        AddDomainInput.Text = "";
        AddDomainForm.Visibility = Visibility.Collapsed;
        AddDomainPrimaryWarning.Visibility = Visibility.Collapsed;
        AddDomainButton.Visibility = Visibility.Visible;
    }

    /// <summary>Add a local mail domain (`admin-dns-add-domain-submit-button` →
    /// <c>LocalDomainAction.AddDomain</c> → <c>fauna.bridges.add_local_domain</c>).
    /// The shared machine trims + lowercases and the nest re-validates RFC-1035 +
    /// is idempotent on the name; an empty input is a no-op (kept consistent with
    /// linux <c>build_add_domain_form</c>). The dispatch refetches the list, so the
    /// new (or already-present) row renders; a rejection surfaces via the snapshot's
    /// error.</summary>
    private async void AddDomainSubmit_Click(object sender, RoutedEventArgs e)
    {
        if (_localDomains is null) return;
        var domain = AddDomainInput.Text.Trim().ToLowerInvariant();
        if (domain.Length == 0) return;
        ResetAddDomainForm();
        await DispatchLocalDomainAsync(new LocalDomainAction.AddDomain(
            domain, DefaultCertMode));
        // No client-side DKIM provision: the nest mints the domain's `default`
        // key as it adds the domain — mail-bridge-lifecycle.md § DKIM
        // provisioning (automatic).
    }

    /// <summary>Soft-delete the clicked domain (`admin-dns-domain-remove-button` →
    /// <c>LocalDomainAction.RemoveDomain</c>). The nest refuses the primary
    /// (`cannot_remove_primary_domain`); the button is disabled there anyway.</summary>
    private async void RemoveDomain_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not DnsDomainItem item) return;
        await DispatchLocalDomainAsync(new LocalDomainAction.RemoveDomain(item.DomainName));
    }

    /// <summary>Restore a soft-deleted domain (`admin-dns-removed-domain-restore-button`
    /// → <c>LocalDomainAction.RestoreDomain</c>), moving it back to the active
    /// list.</summary>
    private async void RestoreDomain_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not DnsRemovedDomainItem item) return;
        await DispatchLocalDomainAsync(new LocalDomainAction.RestoreDomain(item.DomainName));
    }

    /// <summary>Designate (or clear via "None") a domain's catch-all actor
    /// (`admin-dns-domain-catch-all-select` → <c>LocalDomainAction.SetCatchAllActor</c>
    /// → <c>fauna.bridges.set_catch_all_actor</c>; mail-multidomain.md § Per-domain
    /// catch-all). The x:Bind SelectedIndex set when the row is realized raises
    /// SelectionChanged too, so we ignore any change that merely matches the row's
    /// current designation index (the render-time echo) and dispatch only on a
    /// genuine user pick — the same guard linux gets by setting the selection before
    /// connecting the handler, and that <see cref="AutoRenew_Toggled"/> uses. Index 0
    /// = "None" → null actor id (clears).</summary>
    private async void CatchAll_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is not ComboBox cb || cb.DataContext is not DnsDomainItem item) return;
        var idx = cb.SelectedIndex;
        if (idx < 0 || idx == item.CatchAllSelectedIndex) return; // render-time echo
        if (idx >= item.CatchAllActorIds.Count) return;
        var actorId = item.CatchAllActorIds[idx];
        await DispatchLocalDomainAsync(
            new LocalDomainAction.SetCatchAllActor(item.DomainName, actorId));
    }

    /// <summary>Designate (or clear via "Admin (default)") a domain's per-role override
    /// actor (`admin-dns-domain-role-address-&lt;role&gt;-select` →
    /// <c>LocalDomainAction.SetRoleAddress</c> → <c>fauna.bridges.set_role_address</c>;
    /// mail-multidomain.md § Per-domain role-address routing). The four pickers share this
    /// handler: each ComboBox carries its <see cref="RoleAddressPickerItem"/> on
    /// <c>Tag</c>, so the role + actor-id map + current selection come from there (the
    /// row's <see cref="DnsDomainItem"/> is the DataContext, carrying the domain name).
    /// The x:Bind SelectedIndex set when the row is realized raises SelectionChanged too,
    /// so we ignore any change that merely matches the picker's current designation index
    /// (the render-time echo) and dispatch only on a genuine user pick — the same guard
    /// <see cref="CatchAll_SelectionChanged"/> uses. Index 0 = "Admin (default)" → null
    /// actor id (clears; the role falls back to the deployment admin). The nest
    /// atomic-merges, so setting one role preserves the others.</summary>
    private async void RoleAddress_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is not ComboBox cb) return;
        if (cb.DataContext is not DnsDomainItem item) return;
        if (cb.Tag is not RoleAddressPickerItem picker) return;
        var idx = cb.SelectedIndex;
        if (idx < 0 || idx == picker.SelectedIndex) return; // render-time echo
        if (idx >= picker.ActorIds.Count) return;
        var actorId = picker.ActorIds[idx];
        await DispatchLocalDomainAsync(new LocalDomainAction.SetRoleAddress(
            item.DomainName, picker.Kind, actorId));
    }

    /// <summary>Dispatch a <c>LocalDomainAction</c> (the action refetches the list on
    /// success) then re-render. A CRUD rejection is stored in the snapshot's
    /// <c>error</c> (surfaced via <see cref="RenderAll"/>'s precedence); a thrown
    /// <c>DispatchError</c> is shown too. On success the list is hydrated, so flag it
    /// so RenderAll uses the authoritative local-domains snapshot (recovers even if
    /// the initial list-load had failed). No <c>ConfigureAwait(false)</c> — off-thread
    /// bound-state mutation throws a silent COMException.</summary>
    private async Task DispatchLocalDomainAsync(LocalDomainAction action)
    {
        if (_localDomains is null) return;
        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await _localDomains.Dispatch(action);
            _ldHydrated = true;
            _ldLoadError = null;
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
        RenderAll();
    }

    // ── Primary-domain rename (mail-primary-domain-rename.md § UX surface) ───────
    // A dumb render over the existing LocalDomainMachine (no new machine / wire):
    // the snapshot's active_rename projection + rename_available gate drive the
    // per-row affordances + the deployment-wide banner; the four rename actions ride
    // DispatchLocalDomainAsync like every other LocalDomainAction. Mirrors the web +
    // linux reference clients.

    /// <summary>Open the start-a-rename wizard from the PRIMARY row's "Rename primary
    /// domain" button (<c>admin-dns-domain-rename-button</c>) with no pre-target — the
    /// picker defaults to the first promotable domain. Mirrors web
    /// <c>openRenameSheet('')</c>.</summary>
    private void RenamePrimary_Click(object sender, RoutedEventArgs e)
    {
        OpenRenameSheet(null);
    }

    /// <summary>Open the same wizard from a NON-PRIMARY row's "Promote to primary"
    /// shortcut (<c>admin-dns-domain-promote-button</c>), pre-targeting that row's
    /// domain. Mirrors web <c>openRenameSheet(d.domain)</c> / linux's select-by-index.</summary>
    private void PromoteDomain_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not DnsDomainItem item) return;
        OpenRenameSheet(item.DomainName);
    }

    /// <summary>The active non-primary domains offered by the rename picker, captured
    /// when the sheet opens so submit can resolve the chosen display name back to its
    /// opaque <c>domain_id</c> (<c>start_primary_domain_rename</c> is id-keyed, not
    /// name-keyed). Rebuilt on each open from the live snapshot — the wizard never adds
    /// a domain (two-step rule).</summary>
    private List<(byte[] Id, string Name)> _renameTargets = new();

    /// <summary>Open the inline start-a-rename sheet (<c>admin-dns-rename-sheet</c>) —
    /// an inline collapsed Border toggled visible (mirrors the web + linux inline sheet;
    /// a modal <see cref="ContentDialog"/> does NOT surface the sheet to the FlaUI
    /// is_visible tree, so the Border+Visibility idiom the banner uses is the reliable
    /// one). Populates the picker from the current active non-primary domains, optionally
    /// pre-selecting <paramref name="preTargetDomain"/> (the promote shortcut's row).
    /// No-op when there's nothing to promote (the affordance is already gated on
    /// <c>rename_available</c>).</summary>
    private void OpenRenameSheet(string? preTargetDomain)
    {
        if (_localDomains is null) return;
        _renameTargets = _localDomains.Snapshot().active
            .Where(d => !d.isPrimary)
            .Select(d => (Id: d.domainId, Name: d.domain))
            .ToList();
        if (_renameTargets.Count == 0) return; // defensive — gated on rename_available
        RenameNewPrimarySelect.ItemsSource = _renameTargets.Select(t => t.Name).ToList();
        var preIdx = preTargetDomain is null
            ? 0
            : _renameTargets.FindIndex(t => t.Name == preTargetDomain);
        RenameNewPrimarySelect.SelectedIndex = preIdx >= 0 ? preIdx : 0;
        RenameGraceInput.Text = "";
        RenameSheet.Visibility = Visibility.Visible;
    }

    /// <summary>Close the rename sheet without starting a rename
    /// (<c>admin-dns-rename-cancel-button</c>).</summary>
    private void RenameCancel_Click(object sender, RoutedEventArgs e)
    {
        RenameSheet.Visibility = Visibility.Collapsed;
    }

    /// <summary>Dispatch the rename from the open sheet
    /// (<c>admin-dns-rename-submit-button</c> → <c>StartPrimaryRename{new_primary_domain_id,
    /// grace_days}</c>): the picker's selection resolves to its <c>domain_id</c>; the grace
    /// override is blank/non-numeric → <c>null</c> → the nest default 7 (matching linux
    /// <c>parse::&lt;i64&gt;().ok()</c>). The nest re-validates every precondition; a refusal
    /// surfaces via <c>error-message</c>. Collapse first so a raced re-render can't leave the
    /// sheet open over the new in-flight banner.</summary>
    private async void RenameSubmit_Click(object sender, RoutedEventArgs e)
    {
        var idx = RenameNewPrimarySelect.SelectedIndex;
        RenameSheet.Visibility = Visibility.Collapsed;
        if (idx < 0 || idx >= _renameTargets.Count) return;
        var newPrimaryId = _renameTargets[idx].Id;
        long? graceDays = long.TryParse(RenameGraceInput.Text.Trim(), out var g) ? g : (long?)null;
        await DispatchLocalDomainAsync(
            new LocalDomainAction.StartPrimaryRename(newPrimaryId, graceDays));
    }

    /// <summary>Render the deployment-wide in-flight rename banner
    /// (<c>admin-dns-rename-banner</c>) from the snapshot's single
    /// <c>active_rename</c> projection — clear-then-rebuild each render (mirrors linux
    /// <c>build_rename_banner</c>; the reveal-then-confirm reset comes free). Collapsed
    /// (and thus uncounted) when no rename is active. Each lifecycle action is gated on
    /// the rename view's <c>can_*</c> flags and rides <see cref="DispatchLocalDomainAsync"/>
    /// exactly like the pre-flight CRUD actions; the nest owns validation.</summary>
    private void RenderRenameBanner(LocalDomainsSnapshot? ld)
    {
        RenameBannerHost.Children.Clear();
        var r = ld?.activeRename;
        if (r is null)
        {
            RenameBanner.Visibility = Visibility.Collapsed;
            return;
        }
        RenameBanner.Visibility = Visibility.Visible;

        // Head: title + old → new.
        RenameBannerHost.Children.Add(new TextBlock
        {
            Text = S.Get("admin/dns/rename/banner_title"),
            TextWrapping = TextWrapping.Wrap,
        });
        RenameBannerHost.Children.Add(new TextBlock
        {
            Text = $"{r.oldPrimaryDomain} → {r.newPrimaryDomain}",
            FontFamily = new FontFamily("Consolas"),
        });

        // Meta: state + (post-flip only) a client-rendered grace countdown.
        RenameBannerHost.Children.Add(new TextBlock
        {
            Text = $"{S.Get("admin/dns/rename/state_label")} {r.state}",
        });
        if (r.isPostFlipActive && r.graceEndsAt is { } endsAt)
        {
            RenameBannerHost.Children.Add(new TextBlock
            {
                Text = $"{S.Get("admin/dns/rename/grace_ends")} {GraceRemaining(endsAt)}",
            });
        }

        // Complete now — reveal-then-confirm. Offered from ready_to_complete (plain) or
        // grace (force; the confirm names the cache-flush risk). force = can_force_complete.
        if (r.canComplete || r.canForceComplete)
        {
            var force = r.canForceComplete;
            var completeBtn = new Button { Content = S.Get("admin/dns/rename/complete") };
            AutomationProperties.SetAutomationId(completeBtn, Ids.AdminDnsRenameCompleteButton);
            var forceWarn = new TextBlock
            {
                Text = S.Get("admin/dns/rename/complete_force_warning"),
                TextWrapping = TextWrapping.Wrap,
                Visibility = Visibility.Collapsed,
            };
            var confirmBtn = new Button
            {
                Content = S.Get("admin/dns/rename/complete_confirm"),
                Visibility = Visibility.Collapsed,
            };
            AutomationProperties.SetAutomationId(confirmBtn, Ids.AdminDnsRenameCompleteConfirmButton);
            completeBtn.Click += (_, _) =>
            {
                completeBtn.Visibility = Visibility.Collapsed;
                if (force) forceWarn.Visibility = Visibility.Visible;
                confirmBtn.Visibility = Visibility.Visible;
            };
            confirmBtn.Click += async (_, _) =>
                await DispatchLocalDomainAsync(
                    new LocalDomainAction.CompletePrimaryRename(r.renameId, force));
            RenameBannerHost.Children.Add(completeBtn);
            RenameBannerHost.Children.Add(forceWarn);
            RenameBannerHost.Children.Add(confirmBtn);
        }

        // Extend grace by N days (no reveal step) — valid from grace / ready_to_complete.
        if (r.canExtend)
        {
            var extendInput = new TextBox { Text = "7", Width = 80 };
            AutomationProperties.SetAutomationId(extendInput, Ids.AdminDnsRenameExtendDaysInput);
            var extendBtn = new Button { Content = S.Get("admin/dns/rename/extend") };
            AutomationProperties.SetAutomationId(extendBtn, Ids.AdminDnsRenameExtendButton);
            extendBtn.Click += async (_, _) =>
            {
                if (!long.TryParse(extendInput.Text.Trim(), out var n) || n < 1) return;
                await DispatchLocalDomainAsync(
                    new LocalDomainAction.ExtendPrimaryRenameGrace(r.renameId, n));
            };
            var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
            row.Children.Add(extendInput);
            row.Children.Add(extendBtn);
            RenameBannerHost.Children.Add(row);
        }

        // Abort — reveal-then-confirm (destructive; the confirm names the inverse-re-flip
        // cost when post-flip, i.e. !is_pre_flip). Valid from any non-terminal state.
        if (r.canAbort)
        {
            var abortBtn = new Button { Content = S.Get("admin/dns/rename/abort") };
            AutomationProperties.SetAutomationId(abortBtn, Ids.AdminDnsRenameAbortButton);
            var postFlipWarn = new TextBlock
            {
                Text = S.Get("admin/dns/rename/abort_postflip_warning"),
                TextWrapping = TextWrapping.Wrap,
                Visibility = Visibility.Collapsed,
            };
            var abortConfirm = new Button
            {
                Content = S.Get("admin/dns/rename/abort_confirm"),
                Visibility = Visibility.Collapsed,
            };
            AutomationProperties.SetAutomationId(abortConfirm, Ids.AdminDnsRenameAbortConfirmButton);
            var namesPostFlipCost = !r.isPreFlip;
            abortBtn.Click += (_, _) =>
            {
                abortBtn.Visibility = Visibility.Collapsed;
                if (namesPostFlipCost) postFlipWarn.Visibility = Visibility.Visible;
                abortConfirm.Visibility = Visibility.Visible;
            };
            abortConfirm.Click += async (_, _) =>
                await DispatchLocalDomainAsync(
                    new LocalDomainAction.AbortPrimaryRename(r.renameId, null));
            RenameBannerHost.Children.Add(abortBtn);
            RenameBannerHost.Children.Add(postFlipWarn);
            RenameBannerHost.Children.Add(abortConfirm);
        }
    }

    /// <summary>The "Nd Nh" (or "Nh" under a day) countdown to <paramref
    /// name="graceEndsAtMs"/> (epoch-millis), or the "grace elapsed" label once
    /// past — computed once per render via the shared <see
    /// cref="ValueFormat.GraceCountdown"/> (a snapshot, not a ticking timer; the
    /// banner rebuilds on every snapshot refresh). value-formatting.md § Grace
    /// countdown.</summary>
    private static string GraceRemaining(long graceEndsAtMs) =>
        ValueFormat.GraceCountdown(graceEndsAtMs, DateTimeOffset.UtcNow.ToUnixTimeMilliseconds())
            ?? S.Get("admin/dns/rename/grace_elapsed");

}

/// <summary>One domain section on the DNS page.</summary>
public sealed class DnsDomainItem
{
    public string DomainName { get; init; } = "";

    /// <summary>Effective mode is "managed" (the shared machine's projection:
    /// opted in AND a held credential covers the domain). Drives the click
    /// target (flip to the opposite) and which label renders.</summary>
    public bool IsManaged { get; init; }

    /// <summary>Localized mode text shown as the button's content and read by the
    /// <c>get_text(admin-dns-domain-mode)</c> e2e contract: "Fauna-managed" /
    /// "Manual".</summary>
    public string ModeLabel { get; init; } = "";

    /// <summary>The primary (onboarding) domain — shows the read-only
    /// <c>admin-dns-domain-primary-badge</c> and disables its remove-button (the
    /// primary cannot be removed; the nest also refuses it).</summary>
    public bool IsPrimary { get; init; }

    /// <summary>Localized "Primary" badge text (the badge is shown only on the
    /// primary row, gated by <see cref="PrimaryBadgeVisibility"/>).</summary>
    public string PrimaryBadgeText { get; init; } = "";

    /// <summary>Visible only on the primary row — bound to the badge's
    /// <c>Visibility</c> (x:Bind to a Visibility property; no converter).</summary>
    public Visibility PrimaryBadgeVisibility { get; init; } = Visibility.Collapsed;

    /// <summary>Localized remove-button label ("Remove").</summary>
    public string RemoveLabel { get; init; } = "";

    /// <summary>Remove is enabled on every active row except the primary.</summary>
    public bool RemoveEnabled { get; init; } = true;

    // ── Primary-domain rename (mail-primary-domain-rename.md § UX surface) ───────
    /// <summary>Localized "Rename primary domain" button label.</summary>
    public string RenameButtonLabel { get; init; } = "";
    /// <summary>The rename-button is shown only on the primary row (the non-primary
    /// rows carry <c>admin-dns-domain-promote-button</c> instead). Mirrors web
    /// <c>{#if d.is_primary}</c>.</summary>
    public Visibility RenameButtonVisibility { get; init; } = Visibility.Collapsed;
    /// <summary>The primary row's rename-button enables iff a non-primary domain
    /// exists to promote (<c>rename_available</c>, the two-step rule) AND no rename
    /// is already in flight (<c>active_rename == null</c>) — the same gate as web
    /// (<c>disabled={!renameAvailable || !!renameActive}</c>) / linux
    /// (<c>set_sensitive(rename_available &amp;&amp; active_rename.is_none())</c>).</summary>
    public bool RenameEnabled { get; init; }
    /// <summary>Localized "Promote to primary" shortcut label.</summary>
    public string PromoteButtonLabel { get; init; } = "";
    /// <summary>The promote-button is shown on each non-primary active row, and only
    /// while no rename is in flight (hidden entirely during a rename, mirroring web's
    /// <c>{:else if !renameActive}</c>).</summary>
    public Visibility PromoteButtonVisibility { get; init; } = Visibility.Collapsed;
    /// <summary>Shown on the primary row only while a rename is active — the read-only
    /// "Renaming to &lt;new&gt; (&lt;state&gt;)" badge (<see cref="RenameStateText"/>).</summary>
    public Visibility RenameStateVisibility { get; init; } = Visibility.Collapsed;
    /// <summary>The in-flight rename state badge text ("Renaming to new.example
    /// (grace)") — the raw wire state string is rendered verbatim (a forward-compat
    /// state a newer nest wrote still displays). Read via <c>get_text</c>, so its
    /// TextBlock carries an AutomationId only (no Name).</summary>
    public string RenameStateText { get; init; } = "";

    public IReadOnlyList<DnsRecordItem> Records { get; init; } = Array.Empty<DnsRecordItem>();

    // ── Slice 1 — served-cert health badge (admin-dns-cert-status) ──────────────
    /// <summary>The nest-computed served-cert health text (valid / renew-needed /
    /// expiring, with self-signed or expiry-date sub-label); "checking" until
    /// RefreshCertStatus returns.</summary>
    public string CertStatusText { get; init; } = "";
    /// <summary>Brush colouring the cert-status badge (success / caution / neutral).</summary>
    public Brush? CertStatusBrush { get; init; }

    // ── Slice 3 — issuance + manual paste ───────────────────────────────────────
    /// <summary>Get/renew-certificate button label.</summary>
    public string IssueLabel { get; init; } = "";
    /// <summary>Issue is inert while a manual order is pending for this domain (one
    /// order at a time).</summary>
    public bool IssueEnabled { get; init; } = true;
    /// <summary>True for managed/delegated domains (single IssueCert); false for a
    /// manual undelegated domain (two-phase BeginManualIssueCert).</summary>
    public bool IsSingleIssue { get; init; }
    /// <summary>The manual-paste surface (instructions + challenge cards +
    /// complete/cancel) shows only while a manual order awaits this domain.</summary>
    public Visibility PendingVisibility { get; init; } = Visibility.Collapsed;
    public string PasteInstructions { get; init; } = "";
    /// <summary>The transient `_acme-challenge` TXT(s) to paste — the same
    /// admin-dns-record card the matrix uses.</summary>
    public IReadOnlyList<DnsRecordItem> PendingChallenges { get; init; } = Array.Empty<DnsRecordItem>();
    public string CompleteLabel { get; init; } = "";
    public string CancelLabel { get; init; } = "";

    // ── Slice 2 — CNAME renewal-delegation ──────────────────────────────────────
    public bool IsDelegated { get; init; }
    /// <summary>Shown when a delegation exists: the "renewals automated" label +
    /// remove affordance + the one-time CNAME card.</summary>
    public Visibility DelegatedVisibility { get; init; } = Visibility.Collapsed;
    /// <summary>Shown when no delegation exists: the delegate reveal button + form.</summary>
    public Visibility UndelegatedVisibility { get; init; } = Visibility.Visible;
    public string RenewalsAutomatedLabel { get; init; } = "";
    public string RemoveDelegationLabel { get; init; } = "";
    /// <summary>The one-time CNAME the admin sets once (0 or 1 admin-dns-record).</summary>
    public IReadOnlyList<DnsRecordItem> DelegationCname { get; init; } = Array.Empty<DnsRecordItem>();
    public string DelegateLabel { get; init; } = "";
    /// <summary>Delegate is enabled only when a held credential covers some zone.</summary>
    public bool DelegateEnabled { get; init; }
    /// <summary>Help shown (as the delegate button's tooltip) only when delegation is
    /// disabled because no held credential covers a zone; null when delegation is possible.
    /// Mirrors web/linux/apple, which surface admin/dns/cert/delegate_no_zones on hover.</summary>
    public string? DelegateNoZonesLabel { get; init; }
    public string DelegateZoneLabel { get; init; } = "";
    /// <summary>The held credentials' covered zones — the delegate-zone-select
    /// options.</summary>
    public IReadOnlyList<string> DelegateZones { get; init; } = Array.Empty<string>();
    public string DelegateSubmitLabel { get; init; } = "";
    public string DelegateCancelLabel { get; init; } = "";

    // ── Slice 4 — auto-renew checkbox (admin-dns-domain-auto-renew) ──────────────
    public string AutoRenewLabel { get; init; } = "";
    /// <summary>Shown only for managed/delegated domains (the only kind that can
    /// auto-issue).</summary>
    public Visibility AutoRenewVisibility { get; init; } = Visibility.Collapsed;
    /// <summary>Default-on for managed/delegated domains (opt-out persists in
    /// DnsConfig.auto_renew_off).</summary>
    public bool AutoRenewOn { get; init; }
    /// <summary>"on"/"off" mirror of the checked state, surfaced to the e2e via
    /// AutomationProperties.HelpText (the FlaUI bridge maps the `state` attr to
    /// HelpText; the label is the constant "Auto-renew").</summary>
    public string AutoRenewState { get; init; } = "off";

    // ── Per-domain catch-all picker (admin-dns-domain-catch-all-select) ──────────
    /// <summary>"Catch-all:" caption shown beside the picker.</summary>
    public string CatchAllLabel { get; init; } = "";
    /// <summary>Picker option display texts: index 0 = "None", then one per actor
    /// (label), plus an optional trailing "actor …" for a paginated-out designation.
    /// The FlaUI bridge selects an option by this text.</summary>
    public IReadOnlyList<string> CatchAllOptions { get; init; } = Array.Empty<string>();
    /// <summary>Parallel actor-id map for <see cref="CatchAllOptions"/>: null at index
    /// 0 (None / clear), the actor id at each other index. The SelectionChanged
    /// handler dispatches SetCatchAllActor with this id.</summary>
    public IReadOnlyList<byte[]?> CatchAllActorIds { get; init; } = Array.Empty<byte[]?>();
    /// <summary>The currently-designated option's index (0 = None). Bound to the
    /// ComboBox SelectedIndex; the handler ignores a SelectionChanged that matches it
    /// (the render-time echo).</summary>
    public int CatchAllSelectedIndex { get; init; }
    /// <summary>Shown only when the authoritative local-domains row is in (the
    /// designation is known); collapsed on the read-only DNS-fallback row.</summary>
    public Visibility CatchAllVisibility { get; init; } = Visibility.Collapsed;
    /// <summary>Read-only explainer text shown when a SUCCESSION (not an admin) last
    /// cleared this domain's catch-all (`admin-dns-domain-catch-all-cleared-state`,
    /// row 242, `succession-aftermath.md` § Re-key scope) — tells the admin why the
    /// picker above reads "None" and that unmatched mail is now bouncing;
    /// re-designating via that same picker is the fix. Same read-only-explainer idiom
    /// as <see cref="RenameStateText"/> (linux's `build_domain_section` /
    /// `ADMIN_DNS_DOMAIN_CATCH_ALL_CLEARED_STATE` is the reference).</summary>
    public string CatchAllClearedText { get; init; } = "";
    /// <summary>Shown only when `LocalDomainView.catchAllClearedBySuccessionAt` is
    /// non-null — never on the read-only DNS-fallback row, which knows nothing about
    /// succession history.</summary>
    public Visibility CatchAllClearedVisibility { get; init; } = Visibility.Collapsed;

    // ── Per-domain role-address pickers (admin-dns-domain-role-address-<role>-select) ──
    /// <summary>"Role addresses:" caption shown above the four role pickers.</summary>
    public string RoleAddressLabel { get; init; } = "";
    /// <summary>Shown on the same authoritative-row knowledge as the catch-all picker;
    /// collapsed on the read-only DNS-fallback row.</summary>
    public Visibility RoleAddressVisibility { get; init; } = Visibility.Collapsed;
    /// <summary>The four overridable role pickers (postmaster / abuse / noc / security).
    /// Each is bound to its ComboBox's ItemsSource + SelectedIndex and carried on the
    /// ComboBox Tag so the shared <c>RoleAddress_SelectionChanged</c> handler resolves the
    /// role + actor-id map + current selection without a switch.</summary>
    public RoleAddressPickerItem? PostmasterPicker { get; init; }
    public RoleAddressPickerItem? AbusePicker { get; init; }
    public RoleAddressPickerItem? NocPicker { get; init; }
    public RoleAddressPickerItem? SecurityPicker { get; init; }
}

/// <summary>One per-domain role-address override picker
/// (`admin-dns-domain-role-address-&lt;role&gt;-select`). Index 0 = "Admin (default)"
/// (null actor id → clear, the role falls back to the deployment admin); index i>0 = an
/// actor. Carried on its ComboBox's <c>Tag</c> so the shared handler dispatches
/// <c>SetRoleAddress(domain, Role, ActorIds[idx])</c>.</summary>
public sealed class RoleAddressPickerItem
{
    /// <summary>The role's storage key ("postmaster" / "abuse" / "noc" / "security") —
    /// the same token as the `role_address_overrides` JSON key, the ui.yaml ID suffix,
    /// and the shared table's <see cref="RoleAddressOption.key"/>.</summary>
    public string RoleKey { get; init; } = "";
    /// <summary>The role this picker designates, carried alongside <see cref="RoleKey"/>
    /// straight from the shared table so the dispatch handler needs no re-derivation
    /// (<c>RoleAddressKind</c> is UniFFI-<c>internal</c>, not public, but
    /// <c>FaunaApp</c> sees <c>FaunaApp.Core</c> internals via
    /// <c>InternalsVisibleTo</c>).</summary>
    internal RoleAddressKind Kind { get; init; }
    /// <summary>The per-row "<c>&lt;role&gt;@</c>" label (e.g. "postmaster@").</summary>
    public string RoleLabel { get; init; } = "";
    /// <summary>Picker option display texts: index 0 = "Admin (default)", then one per
    /// actor (label), plus an optional trailing "actor …" for a paginated-out
    /// designation. The FlaUI bridge selects an option by this text.</summary>
    public IReadOnlyList<string> Options { get; init; } = Array.Empty<string>();
    /// <summary>Parallel actor-id map for <see cref="Options"/>: null at index 0 (clear),
    /// the actor id at each other index. The handler dispatches SetRoleAddress with this
    /// id.</summary>
    public IReadOnlyList<byte[]?> ActorIds { get; init; } = Array.Empty<byte[]?>();
    /// <summary>The currently-designated option's index (0 = Admin default). Bound to the
    /// ComboBox SelectedIndex; the handler ignores a SelectionChanged that matches it (the
    /// render-time echo).</summary>
    public int SelectedIndex { get; init; }
}

/// <summary>One soft-deleted (recently-removed) domain row in the
/// <c>admin-dns-removed-domain</c> list — the domain name + a Restore button
/// (30-day recovery, mail-multidomain.md § Re-add within 30 days).</summary>
public sealed class DnsRemovedDomainItem
{
    public string DomainName { get; init; } = "";
    public string RestoreLabel { get; init; } = "";
}

/// <summary>One required DNS record row within a domain section.</summary>
public sealed class DnsRecordItem
{
    public string NameCaption { get; init; } = "";
    public string Name { get; init; } = "";
    public string TypeCaption { get; init; } = "";
    public string RecordType { get; init; } = "";
    public string ValueCaption { get; init; } = "";
    public string Value { get; init; } = "";
    public string StatusText { get; init; } = "";
    public Brush? StatusBrush { get; init; }
    public string CopyLabel { get; init; } = "";

    /// <summary>The PTR advisory text, empty on every other record type.</summary>
    public string ProviderNote { get; init; } = "";

    /// <summary>Shows <see cref="ProviderNote"/> on PTR rows only.
    ///
    /// <para>A precomputed <c>Visibility</c> rather than an inline
    /// <c>RecordType == "PTR"</c> comparison in the DataTemplate: <c>x:Bind</c> in
    /// this project's WinUI build cannot carry nested calls or expressions — a
    /// documented constraint of the Windows build setup — which is why every
    /// conditional row in this file, e.g. <c>PrimaryBadgeVisibility</c>, is a plain
    /// bound property computed in C#.</para>
    /// </summary>
    public Visibility ProviderNoteVisibility { get; init; } = Visibility.Collapsed;
}

/// <summary>One held DNS-provider credential row in the
/// <c>admin-dns-credentials-list</c> (provider + covered zones + clear). Never
/// carries the secret field values — the add-form is write-only.</summary>
public sealed class DnsCredentialItem
{
    /// <summary>The credential's <c>snapshot().credentials</c> index — the
    /// <c>ClearCredentials</c> key for this row's clear-button.</summary>
    public uint Index { get; init; }
    public string ProviderText { get; init; } = "";
    public string ZonesText { get; init; } = "";
    public string ClearLabel { get; init; } = "";
}
