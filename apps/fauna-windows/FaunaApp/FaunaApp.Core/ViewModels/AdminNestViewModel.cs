using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// One <c>admin-nest-seed-rotate-roster-item</c> row — a current admin who
/// inherits the successor deployment seed, projected from
/// <see cref="FfiSeedRotationInheritor"/> (the shared
/// <c>fauna_client_admin::seed_rotation_confirm_view</c> fold, which never
/// drops a row it cannot name — <c>label</c> already falls back to the
/// canonical short actor id). Mirrors <see cref="MailSpamTrainingRow"/>'s
/// projection shape.
/// </summary>
public sealed class SeedRotateRosterRow
{
    /// <summary><c>admin-nest-seed-rotate-roster-item</c>'s text.</summary>
    public required string Label { get; init; }

    internal static SeedRotateRosterRow From(FfiSeedRotationInheritor v) => new() { Label = v.label };
}

/// <summary>
/// The <c>admin-nest</c> page (admin.md § N Nest) over the WS-RPC seam
/// (<see cref="INestRpcClient"/> → <c>FfiAdminClient</c> / <c>fauna.setup.status</c>;
/// no <c>/admin/api/*</c> HTTP). The home for nest-wide settings that aren't a
/// feature page, introduced by the per-page-services redesign (2026-06-04,
/// admin.md § Admin IA redesign) that removed the standalone <c>admin-services</c>
/// page. One reflective surface:
/// <list type="bullet">
///   <item><b>Admin pairing</b> (<c>admin-service-pairing-toggle</c>) — the one
///         live service flag that survived the redesign: the master switch for
///         user-initiated nest pairing (per-user multi-homing); off ⇒ the nest
///         rejects <c>fauna.pair.add</c>. Driven reflectively by
///         <c>fauna.admin.services.{list,update}</c> name <c>pairing</c>. (Moved
///         off the removed <c>admin-services</c> page; the user-facing link/unlink
///         surface is the <c>nests</c> page — linked-nests.md.)</item>
/// </list>
/// Factory Reset (the danger zone) stays a page-owned imperative action over a
/// fresh <c>FfiNestClient</c> (it needs the local creds the seam doesn't carry),
/// not VM state — see <c>AdminNestPage.xaml.cs</c>. Mirrors linux
/// <c>build_nest_page</c>. The <c>bridge</c>/<c>algorithm</c> flags were dropped
/// (vestigial / service-to-service residue) and the dns master switch lives on
/// <c>admin-dns</c> (<c>admin-dns-manage-all-toggle</c>). The read-only
/// storage-mode indicator this page once also carried was retired with the
/// no-modes cutover (Phase-4 S8.7) — every nest is sealed at rest, so there is
/// nothing left to display.
/// </summary>
public partial class AdminNestViewModel : ObservableObject
{
    private readonly INestRpcClient _rpc;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    /// <summary>Reflective state of the admin <c>pairing</c> flag, set by
    /// <see cref="LoadAsync"/> (and after a successful flip) from the
    /// <c>services.list</c> reply — the page guards the programmatic
    /// <c>ToggleSwitch.IsOn</c> so the reflective set doesn't re-dispatch an
    /// update.</summary>
    [ObservableProperty] private bool _pairingEnabled;

    /// <summary><c>admin-nest-serving-port-input</c> — the admin-set client-facing
    /// API serving port as an editable string (the <c>admin-mail-*</c> port-input
    /// pattern), reflected from <c>fauna.setup.status</c>
    /// (<c>SetupStatusReply.serving_port</c>, default 443) after each load / save,
    /// not a get-RPC. A string while the admin edits; committed (validated) via
    /// <see cref="SaveServingPortAsync"/>.</summary>
    [ObservableProperty] private string _servingPort = "443";

    /// <summary>Deployment wiring (not a choice): whether this nest is fronted by
    /// the <c>:443</c> SNI router (a Docker/cloud deployment), read off
    /// <c>fauna.setup.status</c> (<c>FfiSetupStatus.frontedByRouter</c>) — the SAME
    /// fetch that seeds <see cref="ServingPort"/>. When <c>true</c> the
    /// <c>admin-nest-serving-port</c> field renders read-only: the port is the
    /// router's fixed 443 and a <c>set_serving_port</c> write is rejected nest-side
    /// (<c>fauna.node_policy.serving_port_fronted</c>). <c>false</c> (the default —
    /// a direct-listener desktop / self-hosted / bare-IP box) keeps the field a
    /// genuine admin choice, editable exactly as today. The page gates
    /// <c>IsEnabled</c> + the read-only hint on this bool (<c>nest/common.md</c>
    /// § Serving ports). The cross-app read-only-field polish — web + linux +
    /// android already landed.</summary>
    [ObservableProperty] private bool _frontedByRouter;

    /// <summary>The localized <c>nest-os-maintenance-status</c> line — the host-OS
    /// patch/reboot status of an onboarded VPS, resolved from the shared
    /// <c>os_maintenance_status_label</c> over the <c>os_*</c> fields on
    /// <c>fauna.setup.status</c> (the SAME fetch that seeds
    /// <see cref="ServingPort"/>). Always present — a nest with no host channel
    /// (dev / desktop) reports the defaults → "OS up to date", no false
    /// alarm. installers/vps.md § Host OS Maintenance § 4.</summary>
    [ObservableProperty] private string _osMaintenanceStatus = string.Empty;

    /// <summary><c>nest-os-updates-count</c> — the raw pending-security-update count
    /// (<c>os_security_updates_pending</c>); the page renders the badge only when
    /// <c>&gt; 0</c>. Split out of the categorical status line (the line carries no
    /// number — the count split, vps.md § 4 / web reference).</summary>
    [ObservableProperty] private uint _osSecurityUpdatesPending;

    /// <summary>Whether the host has a pending reboot (<c>os_reboot_pending</c>); the
    /// page renders <c>nest-os-restart-now-button</c> only when <c>true</c>.</summary>
    [ObservableProperty] private bool _osRebootPending;

    /// <summary><c>admin-nest-region-input</c>'s re-seed value — the declared code,
    /// or <c>null</c> when nothing is declared (admin.md § N Nest → Declared region).
    /// Set by <see cref="LoadAsync"/> and after every <see cref="SetRegionAsync"/>
    /// round-trip.</summary>
    [ObservableProperty] private string? _regionDeclared;

    /// <summary><c>admin-nest-region-status</c> — resolved from the shared
    /// <c>admin_region_view</c> fold. A NORMAL state when undeclared (never blank,
    /// never <c>error-message</c>); the unreadable-declaration arm routes here too,
    /// not to the error surface (the fold's own decision — see
    /// <c>fauna_client_admin::admin_region_view</c>).</summary>
    [ObservableProperty] private string _regionStatus = string.Empty;

    /// <summary><c>admin-nest-region-authority</c> — <c>null</c> exactly when nothing
    /// is declared and no feature-policy document still binds; present otherwise (a
    /// stored document outranks a lost registry enrolment).</summary>
    [ObservableProperty] private string? _regionAuthority;

    /// <summary><c>admin-nest-region-staleness</c> — <c>null</c> unless the nest
    /// reports its authority channel unreached. The rules already received stay in
    /// force; this is a caveat, never an outage.</summary>
    [ObservableProperty] private string? _regionStaleness;

    /// <summary>Whether <c>admin-nest-region-withdraw-button</c> paints at all
    /// (true exactly while a region is declared).</summary>
    [ObservableProperty] private bool _regionCanWithdraw;

    /// <summary>The deployment-identity rotation ceremony's arm state
    /// (box-recovery.md § Deployment-seed rotation) — mirrors linux's
    /// <c>SeedRotateConfirmState</c> / android's <c>SeedRotateConfirmState</c>
    /// three-state shape exactly. <c>None</c> = un-armed (the confirm surface is
    /// absent, not merely disabled).</summary>
    public enum SeedRotateArmStage { None, Loading, Failed, Ready }

    [ObservableProperty] private SeedRotateArmStage _seedRotateStage = SeedRotateArmStage.None;

    /// <summary><c>admin-nest-seed-rotate-roster-reason</c> — why the confirm is
    /// withheld: the roster-loading caption while <see cref="SeedRotateStage"/> is
    /// <c>Loading</c>, the raw error while <c>Failed</c>, or the resolved
    /// <c>blocked_reason</c> while <c>Ready</c> (<c>null</c> when a non-empty
    /// roster names a confirmable set — the ordering rule's "no reason line
    /// beside a live confirm").</summary>
    [ObservableProperty] private string? _seedRotateReasonText;

    /// <summary>Whether <c>admin-nest-seed-rotate-confirm-button</c> is enabled —
    /// <c>view.can_confirm</c> verbatim, never merely "the roster answered" (a
    /// resolved-but-empty roster is refused too, box-recovery.md's self-refuting
    /// case).</summary>
    [ObservableProperty] private bool _seedRotateCanConfirm;

    /// <summary>The <c>admin-nest-seed-rotate-roster-item</c> rows — painted ONLY
    /// from a resolved roster (<see cref="SeedRotateStage"/> <c>Ready</c>); empty
    /// in every other stage, per box-recovery.md's ordering rule (naming the set
    /// that inherits is a precondition of dispatching, so an empty list beside a
    /// live confirm must never happen).</summary>
    public ObservableCollection<SeedRotateRosterRow> SeedRotateRoster { get; } = new();

    /// <summary><c>admin-nest-seed-rotate-status</c> — the ceremony's own verdict
    /// (<c>fauna_client_config::seed_rotation_verdict</c>), present only after a
    /// confirm dispatch. Not <see cref="Error"/>: the outcome that matters most
    /// (<c>predecessor_marked: false</c>) is a success with a caveat, which the
    /// error surface would misreport.</summary>
    [ObservableProperty] private string? _seedRotateStatus;

    // ── Outside-app sign-in keys (admin-nest-oauth-*, authorization-server.md
    //    § The issuer → Two rotation arms) ─────────────────────────────────────
    //
    // Mirrors tui's AdminState.{oauth_confirm,oauth_status,oauth_in_flight} +
    // OauthKeysRead (apps/fauna-tui/src/admin/mod.rs). The two FFI types this
    // section reads/arms over (FfiIssuerKeyView, FfiIssuerForcedArm) stay
    // PRIVATE fields, never public properties: both are uniffi-bindgen-cs
    // `internal` types, and a `public` property naming one is CS0053 — the
    // property-accessor twin of AccountStateDir.EraseAll's CS0050 (see that
    // method's own remark). Every FFI-shaped field is folded into a
    // page-safe (string/bool) property below before it can be seen outside
    // this class.

    /// The answered key-set read, or <c>null</c> while loading/failed —
    /// <see cref="OauthKeyReason"/> covers both of those, and the ordinary
    /// rotation gesture refuses on this being <c>null</c> exactly like tui's
    /// <c>oauth_keys(state).is_none()</c> guard.
    private FfiIssuerKeyView? _oauthView;

    /// The armed forced arm, or <c>null</c> when un-armed. Carried alongside
    /// the CAPTURED confirm view (<see cref="OauthConfirmSummary"/> /
    /// <see cref="OauthConfirmLabel"/>) rather than re-folded at confirm time —
    /// the shared rule every reference render follows.
    private FfiIssuerForcedArm? _oauthArmedArm;

    /// <summary>The armed forced arm, or <c>null</c> when un-armed — the page
    /// reads this to know which kind to dispatch on confirm and which literal
    /// to re-gate the shared confirm button with when arming.
    /// <c>internal</c>, not <c>public</c> — forced, not chosen: same CS0053
    /// reason <see cref="OpenOauthForcedConfirm"/>'s own remark names.</summary>
    internal FfiIssuerForcedArm? OauthArmedArm => _oauthArmedArm;

    /// <summary><c>admin-nest-oauth-key-item-{n}</c> rows — painted ONLY from
    /// an answered read (<see cref="_oauthView"/> non-null); empty in every
    /// other state, per ui.yaml's "never an empty list that would read as
    /// 'no keys'" rule (the reason line covers those states instead).</summary>
    public ObservableCollection<string> OauthKeyRows { get; } = new();

    /// <summary><c>admin-nest-oauth-key-reason</c> — why no key rows are
    /// painted: the read is in flight, or it failed. <c>null</c> exactly when
    /// <see cref="_oauthView"/> is non-null (the rows paint instead).</summary>
    [ObservableProperty] private string? _oauthKeyReason;

    /// <summary>The ordinary arm's cost, painted as page chrome beside
    /// <c>admin-nest-oauth-rotate-button</c> (no dedicated ui.yaml element —
    /// the button has no confirm, so its cost is stated beside it instead).
    /// <c>null</c> until the key set has answered.</summary>
    [ObservableProperty] private string? _oauthRotateCost;

    /// <summary>Whether any of the three sign-in-key controls' calls is in
    /// flight — all three render disabled (never hidden) while true, mirroring
    /// tui's <c>oauth_in_flight</c> guard.</summary>
    [ObservableProperty] private bool _oauthInFlight;

    /// <summary>Whether the forced-arm confirm is armed at all — the page
    /// paints <c>admin-nest-oauth-confirm-summary</c> / <c>-confirm-button</c> /
    /// <c>-cancel-button</c> only while this is <c>true</c>.</summary>
    [ObservableProperty] private bool _oauthConfirmArmed;

    /// <summary><c>admin-nest-oauth-confirm-summary</c> — the armed arm's
    /// cost, captured at ARM time (never re-folded while armed).</summary>
    [ObservableProperty] private string? _oauthConfirmSummary;

    /// <summary><c>admin-nest-oauth-confirm-button</c>'s label — captured
    /// alongside <see cref="OauthConfirmSummary"/>.</summary>
    [ObservableProperty] private string? _oauthConfirmLabel;

    /// <summary><c>admin-nest-oauth-status</c> — the last sign-in-key
    /// control's verdict. Present only after a control was used; deliberately
    /// not <see cref="Error"/> — every success here has consequences worth
    /// words, and a failure must not claim nothing changed (a reply lost to a
    /// timeout can follow a committed rotation).</summary>
    [ObservableProperty] private string? _oauthStatus;

    /// <summary>Whether the three sign-in-key controls should render enabled
    /// — the set has answered and no call is in flight (ui.yaml's shared gate
    /// for all three). A plain computed read, not <c>[ObservableProperty]</c>:
    /// nothing in this page live-binds to VM properties (every render is the
    /// page's own imperative <c>RenderOauth</c>, matching every other section
    /// here), so no change notification is needed.</summary>
    public bool OauthControlsLive => _oauthView is not null && !OauthInFlight;

    internal AdminNestViewModel(INestRpcClient rpc)
    {
        _rpc = rpc;
    }

    /// <summary>Load the admin pairing flag + serving-port / host-OS-maintenance
    /// state.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            var flags = await _rpc.AdminServicesListAsync();
            PairingEnabled = flags.pairing;

            var status = await _rpc.SetupStatusAsync();
            ServingPort = status.servingPort.ToString();
            FrontedByRouter = status.frontedByRouter;
            ApplyOsMaintenance(status);

            // Declared region (fauna.admin.region.get) — folded onto the SAME
            // nav-edge read as pairing/serving-port above (admin.md § N Nest →
            // Declared region), so every nest-page
            // write re-reads it.
            ApplyRegion(await _rpc.AdminRegionStatusAsync());

            // Outside-app sign-in keys (fauna.oauth.issuer_key_status) — folded
            // onto the same nav-edge read, but NEVER through this try's catch: a
            // failed key-set read is the section's OWN reason line (mirrors
            // tui's read_oauth_keys), not this page's Error, so any read fault
            // still shows every other section.
            await LoadOauthKeysAsync();
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }

        // The abuse-report queue (moderation.md § User-initiated reporting → Where
        // it lands) is read in the page's load too, with its own `loaded` bit and
        // reason line — NEVER through the catch above, so a faulted queue read still
        // shows every other section (and an earlier fault still shows the queue).
        await LoadReportsAsync();
    }

    /// <summary>Flip the admin <c>pairing</c> flag and re-read <c>services.list</c>
    /// so the reflective state shows the applied flag (proves the round-trip, not an
    /// optimistic flip — matches linux + the change-tier pattern). On failure the
    /// flag is re-read so the toggle snaps back to server truth and
    /// <see cref="Error"/> is set.</summary>
    public async Task SetPairingAsync(bool enabled)
    {
        Error = null;
        try
        {
            await _rpc.AdminServicesUpdateAsync("pairing", enabled);
            var flags = await _rpc.AdminServicesListAsync();
            PairingEnabled = flags.pairing;
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
            // Re-sync to server truth so the UI doesn't keep the failed optimistic flip.
            try
            {
                var flags = await _rpc.AdminServicesListAsync();
                PairingEnabled = flags.pairing;
            }
            catch
            {
                // Keep the original error; a second failure adds no signal.
            }
        }
    }

    /// <summary>Validate + commit the admin-set client-facing serving port
    /// (<c>admin-nest-serving-port-save-button</c> → <c>fauna.admin.set_serving_port</c>,
    /// Admin-class). The port is a u16 in <c>[1, 65535]</c>; a malformed value
    /// surfaces the <c>serving_port_invalid</c> message in <c>error-message</c> and
    /// does NOT round-trip the RPC (mirrors AdminCalendarViewModel.SaveCaldavPortAsync
    /// / the web reference). On a valid value the write goes over the seam, then the
    /// nest's persisted state is re-read from <c>fauna.setup.status</c> so
    /// <see cref="ServingPort"/> reflects the stored port (reflective, not optimistic
    /// — matches <see cref="SetPairingAsync"/>). The nest applies the new port on its
    /// next restart (it cannot hot-rebind its own listener); no client is stranded
    /// (nest/common.md § Serving ports).</summary>
    public async Task SaveServingPortAsync()
    {
        // Validate locally before dispatching via the shared validator
        // (fauna_core::format::parse_port over the value-format FFI face): a u16 in
        // [1, 65535], trimming whitespace and rejecting 0 / non-numeric / empty /
        // out-of-range. A null return surfaces the page error and skips the RPC.
        if (uniffi.fauna_ffi.FaunaFfiMethods.ParsePort(ServingPort) is not ushort port)
        {
            Error = Strings.Get("admin/nest_page/serving_port_invalid");
            return;
        }
        Error = null;
        try
        {
            await _rpc.SetServingPortAsync(port);
            // Re-read persisted state (reflective round-trip proof, not optimistic).
            var status = await _rpc.SetupStatusAsync();
            ServingPort = status.servingPort.ToString();
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
            // Re-sync to server truth so the field doesn't keep an unsaved value.
            try
            {
                var status = await _rpc.SetupStatusAsync();
                ServingPort = status.servingPort.ToString();
            }
            catch
            {
                // Keep the original error; a second failure adds no signal.
            }
        }
    }

    /// <summary>Dispatch the admin "restart now" (<c>fauna.admin.request_host_restart</c>):
    /// the nest writes a <c>restart-requested</c> flag the host reboot-coordinator
    /// consumes on its next run (rebooting regardless of idle/ceiling). After the
    /// write the <c>os_*</c> state is re-read from <c>fauna.setup.status</c> so the
    /// indicator reflects any change (reflective, not optimistic — mirrors
    /// web/linux/android/apple; the box actually reboots on the next coordinator
    /// run). A <c>no_host</c> rejection (a nest with no maintenance mount) routes to
    /// <see cref="Error"/>. installers/vps.md § Host OS Maintenance § 4.</summary>
    public async Task RestartNowAsync()
    {
        Error = null;
        try
        {
            await _rpc.RequestHostRestartAsync();
            // Re-read persisted os_* state (round-trip proof, not optimistic).
            ApplyOsMaintenance(await _rpc.SetupStatusAsync());
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
    }

    /// <summary>Declare/re-declare (<paramref name="region"/> already validated by
    /// the page via <c>FaunaFfiMethods.AdminParseRegionCode</c>) or withdraw
    /// (<paramref name="region"/> null) via <c>fauna.admin.region.set</c>, then
    /// re-read <c>fauna.admin.region.get</c> so the section re-seeds from the
    /// persisted declaration — non-optimistic, matching
    /// <see cref="SetPairingAsync"/>/<see cref="SaveServingPortAsync"/> (a
    /// re-declaration also retires the previous region's feature-policy document
    /// nest-side, so this is never a mere toggle). Mirrors apple's
    /// <c>AdminNestVM.setRegion</c>.</summary>
    public async Task SetRegionAsync(string? region)
    {
        Error = null;
        try
        {
            await _rpc.SetRegionAsync(region);
            ApplyRegion(await _rpc.AdminRegionStatusAsync());
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
            // Re-sync to server truth so the section doesn't keep a failed write.
            try
            {
                ApplyRegion(await _rpc.AdminRegionStatusAsync());
            }
            catch
            {
                // Keep the original error; a second failure adds no signal.
            }
        }
    }

    /// <summary>Fold a <c>fauna.admin.region.get</c> reply into the page's plain
    /// (already-resolved) state — every rendering decision was already made by
    /// the shared <c>admin_region_view</c> fold; this only resolves the
    /// <c>LocalizedText</c> fields, never re-derives anything from
    /// <c>declared</c>/<c>staleness</c>.</summary>
    private void ApplyRegion(uniffi.fauna_ffi.FfiAdminRegionView view)
    {
        RegionDeclared = view.declared;
        RegionStatus = Strings.Resolve(view.status);
        RegionAuthority = view.authority is { } authority ? Strings.Resolve(authority) : null;
        RegionStaleness = view.staleness is { } staleness ? Strings.Resolve(staleness) : null;
        RegionCanWithdraw = view.canWithdraw;
    }

    /// <summary>Hydrate the host-OS-maintenance surface from a
    /// <c>fauna.setup.status</c> reply: the raw count + reboot flag (which gate the
    /// <c>nest-os-updates-count</c> badge / <c>nest-os-restart-now-button</c>) and the
    /// shared status line. The informational <c>os_reboot_deferred_since</c> /
    /// <c>os_last_patched_at</c> fields aren't rendered in v1.</summary>
    private void ApplyOsMaintenance(uniffi.fauna_ffi.FfiSetupStatus status)
    {
        OsSecurityUpdatesPending = status.osSecurityUpdatesPending;
        OsRebootPending = status.osRebootPending;
        OsMaintenanceStatus =
            OsMaintenanceStatusLine(status.osSecurityUpdatesPending, status.osRebootPending);
    }

    /// <summary>The localized <c>nest-os-maintenance-status</c> line. The state→key
    /// decision (reboot pending → <c>os_restart_pending</c>; updates, no reboot →
    /// <c>os_updates_pending</c>; else <c>os_up_to_date</c>) is single-sourced in
    /// shared Rust (<c>fauna_core::format::os_maintenance_status_label</c> via the
    /// value-format FFI), so the uniform per-app map can't drift — a pure consume
    /// (the <c>storage_mode_label</c> precedent, priority #2). installers/vps.md
    /// § Host OS Maintenance § 4.</summary>
    internal static string OsMaintenanceStatusLine(uint securityUpdatesPending, bool rebootPending) =>
        Strings.Resolve(
            uniffi.fauna_ffi.FaunaFfiMethods.OsMaintenanceStatusLabel(securityUpdatesPending, rebootPending));

    /// <summary>Arm the deployment-identity rotation ceremony
    /// (<c>admin-nest-seed-rotate-button</c>): paint <see cref="SeedRotateArmStage.Loading"/>
    /// synchronously (the confirm surface must exist, disabled, in the same frame
    /// the arm click replies), then read the roster. Mirrors linux's arm-click
    /// handler / android's <c>armSeedRotate</c> — the synchronous paint is
    /// load-bearing, not stylistic.</summary>
    public async Task ArmSeedRotateAsync()
    {
        SeedRotateStage = SeedRotateArmStage.Loading;
        SeedRotateRoster.Clear();
        SeedRotateCanConfirm = false;
        SeedRotateReasonText = Strings.Get("admin/nest_page/rotate_seed_roster_loading");
        try
        {
            var view = await _rpc.SeedRotateRosterAsync();
            // A late reply after cancel must not silently re-arm — mirrors linux's
            // `set_seed_rotate_roster`'s `if borrow().is_none() { return }` / android's
            // `if (seedRotateConfirm.value !is Loading) return`.
            if (SeedRotateStage != SeedRotateArmStage.Loading) return;
            SeedRotateStage = SeedRotateArmStage.Ready;
            SeedRotateCanConfirm = view.canConfirm;
            SeedRotateReasonText = view.blockedReason is { } reason ? Strings.Resolve(reason) : null;
            foreach (var inheritor in view.inheritors)
            {
                SeedRotateRoster.Add(SeedRotateRosterRow.From(inheritor));
            }
        }
        catch (Exception ex)
        {
            if (SeedRotateStage != SeedRotateArmStage.Loading) return;
            SeedRotateStage = SeedRotateArmStage.Failed;
            SeedRotateReasonText = Strings.Error(ex);
        }
    }

    /// <summary>Disarm (<c>admin-nest-seed-rotate-cancel-button</c>): drop back to
    /// un-armed, touching nothing nest-side.</summary>
    public void CancelSeedRotate()
    {
        SeedRotateStage = SeedRotateArmStage.None;
        SeedRotateRoster.Clear();
        SeedRotateReasonText = null;
        SeedRotateCanConfirm = false;
    }

    /// <summary>Confirm the ceremony (<c>admin-nest-seed-rotate-confirm-button</c>):
    /// disarm SYNCHRONOUSLY before dispatch (a double click must not chain a
    /// second rotation onto the first — a second successor would strand the
    /// first nobody marked), then rotate and paint the verdict. On a successful
    /// rotation nothing further is owed from here: the seed-map fan-out retired
    /// with the plane-era custody (box-recovery.md § The plane-era recovery
    /// floor, (c) The writes). This
    /// call outlives the click: the committed rotation tears down the box's
    /// serving generation, so the app reconnects mid-flight by design, and this
    /// is a plain awaited <c>Task</c> with no budget or cancellation tied to the
    /// click that started it.</summary>
    public async Task ConfirmSeedRotateAsync()
    {
        if (SeedRotateStage != SeedRotateArmStage.Ready || !SeedRotateCanConfirm) return;
        SeedRotateStage = SeedRotateArmStage.None;
        SeedRotateRoster.Clear();
        SeedRotateReasonText = null;
        SeedRotateCanConfirm = false;
        SeedRotateStatus = Strings.Get("admin/nest_page/rotate_seed_working");
        try
        {
            var result = await _rpc.RotateDeploymentSeedAsync();
            SeedRotateStatus = Strings.Resolve(result.verdict);
        }
        catch (Exception ex)
        {
            SeedRotateStatus = Strings.Error(ex);
        }
    }

    /// <summary>Load the outside-app sign-in key set
    /// (<c>fauna.oauth.issuer_key_status</c>), folded through the shared read
    /// — non-fatal to the rest of the page: a failure becomes the section's
    /// own reason line, never <see cref="LoadAsync"/>'s <see cref="Error"/>.
    /// Mirrors tui's <c>read_oauth_keys</c>.</summary>
    private async Task LoadOauthKeysAsync()
    {
        try
        {
            _oauthView = await _rpc.AdminIssuerKeyStatusAsync();
            RefreshOauthKeyRows();
            OauthKeyReason = null;
        }
        catch (Exception ex)
        {
            _oauthView = null;
            OauthKeyRows.Clear();
            OauthRotateCost = null;
            OauthKeyReason = Strings.Format("admin/nest_page/oauth_keys_error", ex.Message);
        }
    }

    /// <summary>Rebuild <see cref="OauthKeyRows"/> + <see cref="OauthRotateCost"/>
    /// from <see cref="_oauthView"/> — the countdown is counted against the
    /// WALL CLOCK AT PAINT (the point of a retired key's line), never the
    /// load-time clock, mirroring tui's <c>Timestamp::now_secs_or_zero()</c>
    /// read inside its own render loop.</summary>
    private void RefreshOauthKeyRows()
    {
        OauthKeyRows.Clear();
        if (_oauthView is not { } view) return;
        long now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        foreach (var row in view.keys)
        {
            OauthKeyRows.Add(Strings.Resolve(FaunaFfiMethods.IssuerKeyRowLabel(row, now)));
        }
        OauthRotateCost = Strings.Resolve(FaunaFfiMethods.IssuerKeyRotateCost(view));
    }

    /// <summary><c>admin-nest-oauth-rotate-button</c>: the ordinary
    /// rotation — mints a new signer, retires the outgoing key. No confirm;
    /// disarms any forced confirm beside it first (a rotation about to
    /// change the key count would leave an armed confirm stating a stale
    /// cost — the same disarm tui's <c>Action::RotateIssuerKey</c> applies).
    /// Refuses on the same two guards tui's arm does: already in flight, or
    /// the key set hasn't answered.</summary>
    public async Task RotateIssuerKeyAsync()
    {
        if (OauthInFlight || _oauthView is null) return;
        CancelOauthForced();
        OauthInFlight = true;
        OauthStatus = Strings.Get("admin/nest_page/oauth_working");
        try
        {
            var verdict = await _rpc.AdminRotateIssuerKeyAsync();
            // The verdict and the re-read land in ONE state update — a
            // verdict published ahead of the re-read would read stale rows
            // (the contract point the first drafts got wrong).
            _oauthView = await _rpc.AdminIssuerKeyStatusAsync();
            RefreshOauthKeyRows();
            OauthKeyReason = null;
            OauthStatus = Strings.Resolve(verdict);
        }
        catch (Exception ex)
        {
            OauthStatus = Strings.Error(ex);
        }
        finally
        {
            OauthInFlight = false;
        }
    }

    /// <summary>Arms the forced arm's inline confirm
    /// (<c>admin-nest-oauth-force-rotate-button</c> /
    /// <c>-secret-force-rotate-button</c>): fold <paramref name="arm"/> into
    /// its confirm view over the CURRENT key set — captured now, never
    /// re-folded while armed. Refuses if a call is in flight or the set
    /// hasn't answered.
    /// <c>internal</c>, not <c>public</c> — forced, not chosen:
    /// <c>FfiIssuerForcedArm</c> is a <c>uniffi-bindgen-cs</c>-generated
    /// <c>internal</c> type, so a <c>public</c> member naming one is CS0051
    /// (the parameter twin of <c>AccountStateDir.EraseAll</c>'s CS0050; see
    /// that method's own remark).</summary>
    internal void OpenOauthForcedConfirm(FfiIssuerForcedArm arm)
    {
        if (OauthInFlight || _oauthView is not { } view) return;
        var confirm = FaunaFfiMethods.IssuerForcedConfirmView(arm, view);
        _oauthArmedArm = arm;
        OauthConfirmSummary = Strings.Resolve(confirm.summary);
        OauthConfirmLabel = Strings.Resolve(confirm.confirmLabel);
        OauthConfirmArmed = true;
        OauthStatus = null;
    }

    /// <summary><c>admin-nest-oauth-cancel-button</c>: disarm, touching
    /// nothing.</summary>
    public void CancelOauthForced()
    {
        _oauthArmedArm = null;
        OauthConfirmArmed = false;
        OauthConfirmSummary = null;
        OauthConfirmLabel = null;
    }

    /// <summary><c>admin-nest-oauth-confirm-button</c>: dispatch exactly the
    /// armed arm's kind. Disarms FIRST (the seed-rotate discipline): a
    /// double press must not dispatch a second forced rotation. <paramref
    /// name="arm"/> must match the ARMED arm — a confirm painted for one arm
    /// pressed after the other was armed dispatches nothing (mirrors tui's
    /// <c>Action::ConfirmOauthForced</c> mismatch guard).</summary>
    internal async Task ConfirmOauthForcedAsync(FfiIssuerForcedArm arm)
    {
        if (_oauthArmedArm != arm || OauthInFlight) return;
        CancelOauthForced();
        OauthInFlight = true;
        OauthStatus = Strings.Get("admin/nest_page/oauth_working");
        try
        {
            var verdict = await _rpc.AdminForceRotateIssuerAsync(arm);
            _oauthView = await _rpc.AdminIssuerKeyStatusAsync();
            RefreshOauthKeyRows();
            OauthKeyReason = null;
            OauthStatus = Strings.Resolve(verdict);
        }
        catch (Exception ex)
        {
            OauthStatus = Strings.Error(ex);
        }
        finally
        {
            OauthInFlight = false;
        }
    }

    // ── Legal takedown console (moderation.md § Legal takedown →
    //     Invocation surface; admin.md § N Nest → Legal takedown console) ────
    //
    // Every decision is pre-made in shared Rust: `FaunaFfiMethods.TakedownFormView`
    // owns the gating (content id present; a legal reference REQUIRED for a
    // takedown, optional on a restore), the arm label, the blocked reason and the
    // confirm summary — a compulsory act is never confirmed blind — and
    // `FaunaFfiMethods.TakedownVerdict` owns the wording of the outcome. This VM
    // paints that fold and dispatches; it decides nothing and words nothing.
    // Mirrors linux's `render_takedown` / `submit_takedown` and tui's
    // `apps/fauna-tui/src/admin/nest.rs`.

    /// The armed form, captured at ARM time and dispatched verbatim on confirm —
    /// never re-read from the inputs, which the operator can still edit while the
    /// confirm is on screen. The same capture-at-arm rule the oauth forced arm and
    /// the seed-rotation ceremony follow.
    private (string ContentId, bool Conversation, string Reference, bool Restore)?
        _takedownArmedForm;

    /// <summary><c>admin-nest-takedown-content-id-input</c> — the content id named
    /// by the legal obligation (a post's 32-byte hex, or a conversation record
    /// cid).</summary>
    [ObservableProperty] private string _takedownContentId = string.Empty;

    /// <summary><c>admin-nest-takedown-type-conversation-radio</c> when true,
    /// <c>admin-nest-takedown-type-post-radio</c> when false (the default —
    /// posts are the common case).</summary>
    [ObservableProperty] private bool _takedownConversation;

    /// <summary><c>admin-nest-takedown-reference-input</c> — the legal reference
    /// (court order, statutory demand). Required to take down, an optional note
    /// on a restore; the shared fold, not this VM, enforces that.</summary>
    [ObservableProperty] private string _takedownReference = string.Empty;

    /// <summary><c>admin-nest-takedown-restore-checkbox</c> — overturn an
    /// existing takedown instead of issuing one.</summary>
    [ObservableProperty] private bool _takedownRestore;

    /// <summary><c>admin-nest-takedown-button</c>'s label — the shared fold's
    /// <c>arm_label</c>, which changes with <see cref="TakedownRestore"/>.</summary>
    [ObservableProperty] private string? _takedownArmLabel;

    /// <summary>Whether <c>admin-nest-takedown-button</c> may arm — the shared
    /// fold's <c>can_submit</c> verbatim, never a local check. The citation-less
    /// takedown is un-armable through exactly this.</summary>
    [ObservableProperty] private bool _takedownCanSubmit;

    /// <summary>Why the arm control is disabled, painted beside it as page chrome
    /// (no dedicated ui.yaml element — a disabled control owes its reason).
    /// <c>null</c> exactly when <see cref="TakedownCanSubmit"/> is true.</summary>
    [ObservableProperty] private string? _takedownBlockedReason;

    /// <summary>Whether the confirm is armed — the page paints
    /// <c>admin-nest-takedown-confirm-summary</c> / <c>-confirm-button</c> /
    /// <c>-cancel-button</c> only while true (absent, not merely disabled).</summary>
    [ObservableProperty] private bool _takedownArmed;

    /// <summary><c>admin-nest-takedown-confirm-summary</c> — names the verb, the
    /// content id and the citation, captured at arm time.</summary>
    [ObservableProperty] private string? _takedownConfirmSummary;

    /// <summary><c>admin-nest-takedown-confirm-button</c>'s label, captured
    /// alongside <see cref="TakedownConfirmSummary"/>.</summary>
    [ObservableProperty] private string? _takedownConfirmLabel;

    /// <summary><c>admin-nest-takedown-status</c> — the dispatch's verdict.
    /// Present only after an attempt; deliberately not <see cref="Error"/>, since
    /// a completed takedown is a success with consequences, not a failure.</summary>
    [ObservableProperty] private string? _takedownStatus;

    // Per-keystroke / per-toggle recompute, the C# twin of linux's
    // `connect_changed` / `connect_toggled` → `refresh_takedown_arm`.
    partial void OnTakedownContentIdChanged(string value) => RefreshTakedownArm();
    partial void OnTakedownConversationChanged(bool value) => RefreshTakedownArm();
    partial void OnTakedownReferenceChanged(string value) => RefreshTakedownArm();
    partial void OnTakedownRestoreChanged(bool value) => RefreshTakedownArm();

    /// <summary>Re-fold the form through the shared view and repaint the arm
    /// control. Pure and synchronous — `takedown_form_view` needs no nest read,
    /// which is why this section has none of the seed-rotation ceremony's
    /// Loading/Failed/Ready staging.</summary>
    public void RefreshTakedownArm()
    {
        var view = FaunaFfiMethods.TakedownFormView(
            TakedownContentId ?? string.Empty,
            TakedownConversation,
            TakedownReference ?? string.Empty,
            TakedownRestore);
        TakedownArmLabel = Strings.Resolve(view.armLabel);
        TakedownCanSubmit = view.canSubmit;
        TakedownBlockedReason =
            view.blockedReason is { } reason ? Strings.Resolve(reason) : null;
    }

    /// <summary><c>admin-nest-takedown-button</c>: capture the form and paint the
    /// confirm. Re-gates on the shared <c>can_submit</c> rather than trusting the
    /// control's enabled state — a disabled WinUI button cannot be Invoked, but
    /// the guard is the contract, not the chrome.</summary>
    public void ArmTakedown()
    {
        var contentId = TakedownContentId ?? string.Empty;
        var reference = TakedownReference ?? string.Empty;
        var view = FaunaFfiMethods.TakedownFormView(
            contentId, TakedownConversation, reference, TakedownRestore);
        if (!view.canSubmit) return;
        _takedownArmedForm = (contentId, TakedownConversation, reference, TakedownRestore);
        TakedownConfirmSummary = Strings.Resolve(view.confirmSummary);
        TakedownConfirmLabel = Strings.Resolve(view.confirmLabel);
        TakedownArmed = true;
        TakedownStatus = null;
    }

    /// <summary><c>admin-nest-takedown-cancel-button</c>: disarm, touching nothing
    /// nest-side.</summary>
    public void CancelTakedown()
    {
        _takedownArmedForm = null;
        TakedownArmed = false;
        TakedownConfirmSummary = null;
        TakedownConfirmLabel = null;
    }

    /// <summary><c>admin-nest-takedown-confirm-button</c>: dispatch the CAPTURED
    /// form. Disarms FIRST and synchronously, before the first await — a second
    /// press must not issue a second takedown, and the e2e asserts the confirm
    /// control is gone the moment it is pressed.</summary>
    public async Task ConfirmTakedownAsync()
    {
        if (_takedownArmedForm is not { } form) return;
        CancelTakedown();
        TakedownStatus = Strings.Get("admin/nest_page/takedown_working");
        string? error = null;
        try
        {
            await _rpc.ModerationLegalTakedownAsync(
                form.ContentId.Trim(), form.Conversation, form.Reference.Trim(),
                form.Restore);
        }
        catch (Exception ex)
        {
            error = ex.Message;
        }
        // One wording seam for both outcomes: the shared verdict fold takes the
        // error (or its absence) and answers the sentence. Never hand-worded, and
        // never routed to `Error` — see TakedownStatus's own remark.
        TakedownStatus = Strings.Resolve(
            FaunaFfiMethods.TakedownVerdict(form.Restore, error));
    }

    // ── User-initiated abuse reports queue (moderation.md § User-initiated
    //     reporting → Where it lands) ─────────────────────────────────────────
    //
    // The takedown console's inbox, after it on the page. Every row arrives
    // already worded from shared Rust (`fauna.moderation.abuse_report.queue`,
    // admin-class): the reason, the origin (a local reporter by handle, a
    // forwarded report as "a user of <nest>" — never a forwarded reporter), whether
    // the row can open the takedown console (posts and messages only). A report is
    // EVIDENCE for the admin to weigh, so resolving a row only RECORDS the outcome;
    // acting is the console or a suspension. Reference: tui's `reports_elements`,
    // web's `reportLine`.

    /// <summary>The open queue rows as the page paints them (flat
    /// <c>admin-nest-report-item[i]</c>); the raw rows stay in
    /// <see cref="_reportRows"/> — UniFFI types are <c>internal</c> and a row
    /// template needs public ones.</summary>
    public ObservableCollection<ReportQueueRowView> Reports { get; } = new();

    private readonly Dictionary<string, FfiReportQueueRow> _reportRows = new();

    /// <summary>Whether a queue read has landed. The loading line paints until it
    /// has and the empty line only after it (ui/README.md § List pages: loading is
    /// not empty).</summary>
    [ObservableProperty] private bool _reportsLoaded;

    /// <summary>The line the last resolve (or a failed queue read) painted; its own
    /// line, never <see cref="Error"/>, so a faulted queue still shows every other
    /// section.</summary>
    [ObservableProperty] private string? _reportsStatus;

    /// <summary>Read the open queue (<c>fauna.moderation.abuse_report.queue</c>) and
    /// repaint it. A failed read says so on <see cref="ReportsStatus"/> and leaves
    /// <see cref="ReportsLoaded"/> as it was — never an empty line off a read that
    /// did not land.</summary>
    public async Task LoadReportsAsync()
    {
        try
        {
            var rows = await _rpc.AbuseReportQueueAsync();
            _reportRows.Clear();
            Reports.Clear();
            foreach (var r in rows)
            {
                _reportRows[r.reportId] = r;
                Reports.Add(MapReportRow(r));
            }
            ReportsLoaded = true;
        }
        catch (Exception ex)
        {
            ReportsStatus = Strings.Format("admin/nest_page/reports_failed", Strings.Error(ex));
        }
    }

    /// <summary><c>admin-nest-report-acted-button</c> (<paramref name="acted"/> true)
    /// / <c>-dismiss-button</c>: record the outcome, say so in the shared verdict's
    /// words (<c>report_resolve_verdict</c>), and re-read the queue — a resolved row
    /// leaves it. The reporter is told the outcome, nothing more.</summary>
    public async Task ResolveReportAsync(string reportId, bool acted)
    {
        string? failure = null;
        try
        {
            await _rpc.AbuseReportResolveAsync(reportId, acted);
        }
        catch (Exception ex)
        {
            failure = Strings.Error(ex);
        }
        ReportsStatus = Strings.Resolve(FaunaFfiMethods.ReportResolveVerdict(acted, failure));
        await LoadReportsAsync();
    }

    /// <summary><c>admin-nest-report-open-takedown-button</c>: pre-fill the takedown
    /// console from the row (the shared <c>report_takedown_prefill</c>) with NO
    /// citation — the console's own guard stands, so a pre-filled takedown is still
    /// un-armable until the admin names a legal reference. Returns whether the row had
    /// a takedown to open (an account has none).</summary>
    public bool OpenReportTakedown(string reportId)
    {
        if (!_reportRows.TryGetValue(reportId, out var row)) return false;
        if (FaunaFfiMethods.ReportTakedownPrefill(row.subject) is not { } prefill) return false;
        CancelTakedown();
        TakedownContentId = prefill.contentId;
        TakedownConversation = prefill.conversation;
        TakedownReference = string.Empty;
        TakedownRestore = false;
        TakedownStatus = null;
        RefreshTakedownArm();
        return true;
    }

    /// <summary>One queue row's line — <c>reason · kind id · origin · when — note —
    /// “excerpt”</c> — the same join web's <c>reportLine</c> and tui's
    /// <c>reports_elements</c> make (the e2e reads it); every part but the join is
    /// shared Rust's, and the time is the shared fixed local render.</summary>
    internal static ReportQueueRowView MapReportRow(FfiReportQueueRow r)
    {
        var id = ReportSheetViewModel.SubjectIdOf(r.subject);
        var kind = ReportSheetViewModel.SubjectKindOf(r.subject);
        var text =
            $"{Strings.Resolve(r.reason)} · {kind} {id} · {Strings.Resolve(r.origin)} · " +
            FaunaFfiMethods.FormatUnixLocal(r.createdAt / 1_000_000);
        if (!string.IsNullOrEmpty(r.note)) text += $" — {r.note}";
        if (!string.IsNullOrEmpty(r.excerpt)) text += $" — “{r.excerpt}”";
        return new ReportQueueRowView(r.reportId, id, kind, text, r.canOpenTakedown);
    }
}

/// <summary>
/// One open row of the admin's abuse-report queue (<c>admin-nest-report-item</c>):
/// the id the three buttons act on, the subject it names, the shared-worded line the
/// e2e reads, and whether <c>admin-nest-report-open-takedown-button</c> paints
/// (posts and messages only).
/// </summary>
public record ReportQueueRowView(
    string ReportId, string SubjectId, string Kind, string Line, bool CanOpenTakedown);
