using Xunit;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the <c>admin-nest</c> page VM, over the
/// <see cref="MockNestRpcClient"/> WS-RPC seam (admin.md § N Nest — the nest-wide
/// settings that aren't a feature page: the admin pairing toggle; no live
/// nest / FlaUI, which flakes on win-arm64). The Factory Reset danger zone is
/// page-owned imperative glue (a fresh FfiNestClient over local creds), not VM
/// state, so it isn't exercised here. Replaces the pairing half of the retired
/// <c>AdminServicesViewModelTests</c>. (The storage-mode indicator this page
/// once also carried was retired entirely with the no-modes cutover, Phase-4
/// S8.7 — every nest is sealed at rest.)
/// </summary>
public class AdminNestViewModelTests
{
    [Fact]
    public async Task Load_PopulatesPairingFlag()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(
                bridge: true, pairing: true),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
        };
        var vm = new AdminNestViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.PairingEnabled);
        Assert.Null(vm.Error);
        Assert.False(vm.IsLoading);
        // Both reads went over the WS-RPC seam (no HTTP twin).
        Assert.Contains("AdminServicesList", rpc.Calls);
        Assert.Contains("SetupStatus", rpc.Calls);
    }

    [Fact]
    public async Task SetPairing_FlipsFlagThenRefetches()
    {
        // The pairing knob is the admin master switch for user-initiated nest
        // pairing (admin.md § N Nest; off ⇒ the nest rejects fauna.pair.add).
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(true, true),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetPairingAsync(false);

        Assert.NotNull(rpc.LastServiceUpdate);
        Assert.Equal("pairing", rpc.LastServiceUpdate!.Value.Name);
        Assert.False(rpc.LastServiceUpdate!.Value.Enabled);
        // update is followed by a services.list refetch (reflective, not optimistic).
        var calls = rpc.Calls.ToList();
        int upd = calls.LastIndexOf("AdminServicesUpdate");
        int list = calls.LastIndexOf("AdminServicesList");
        Assert.True(upd >= 0 && list > upd,
            $"expected a services refetch after update; calls=[{string.Join(",", calls)}]");
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task SetPairing_FailureRoutesToPageError()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        rpc.NextError = "boom";
        await vm.SetPairingAsync(true);

        // Pairing failures route to the page-level error-message surface (admin.md
        // § N Nest), not an unhandled throw. i18n strings aren't loaded in the test
        // host, so assert the surface is populated rather than the exact text.
        Assert.False(string.IsNullOrEmpty(vm.Error));
    }

    [Fact]
    public async Task Load_PopulatesServingPortFromSetupStatus()
    {
        // The admin-set client-facing serving port (admin-nest § Serving ports)
        // hydrates from fauna.setup.status (SetupStatusReply.serving_port) as an
        // editable string, NOT a get-RPC.
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(9443),
        };
        var vm = new AdminNestViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("9443", vm.ServingPort);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task SaveServingPort_Valid_WritesThenRereads()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(443),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        // After the write the nest re-reads persisted state (here 8443).
        rpc.NextSetupStatus = MockNestRpcClient.MakeSetupStatus(8443);
        vm.ServingPort = "8443";
        await vm.SaveServingPortAsync();

        // The write rode the WS-RPC seam (fauna.admin.set_serving_port), then a
        // setup.status refetch reflected persisted state (reflective, not optimistic).
        Assert.Equal((ushort)8443, rpc.LastServingPort);
        Assert.Equal("8443", vm.ServingPort);
        var calls = rpc.Calls.ToList();
        int set = calls.LastIndexOf("SetServingPort");
        int status = calls.LastIndexOf("SetupStatus");
        Assert.True(set >= 0 && status > set,
            $"expected a setup.status refetch after the write; calls=[{string.Join(",", calls)}]");
        Assert.Null(vm.Error);
    }

    [Theory]
    [InlineData("0")]        // below [1, 65535]
    [InlineData("65536")]    // above [1, 65535]
    [InlineData("abc")]      // not a number
    [InlineData("")]         // empty
    [InlineData("-1")]       // negative (shared parse_port rejection)
    [InlineData("84.43")]    // fractional (shared parse_port rejection)
    [InlineData("8 443")]    // interior whitespace (shared parse_port rejection)
    public async Task SaveServingPort_Invalid_SetsError_AndSkipsWrite(string bad)
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(443),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.ServingPort = bad;
        await vm.SaveServingPortAsync();

        // A malformed port surfaces the page error and does NOT round-trip the RPC
        // (mirrors AdminCalendarViewModel.SaveCaldavPortAsync / the web reference).
        Assert.Null(rpc.LastServingPort);
        Assert.False(string.IsNullOrEmpty(vm.Error));
    }

    [Fact]
    public async Task Load_DirectListener_LeavesServingPortEditable()
    {
        // The default (fronted_by_router == false) is a direct-listener desktop /
        // self-hosted / bare-IP box, where serving_port is a genuine admin choice —
        // the field stays editable exactly as today (nest/common.md § Serving ports).
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(443, frontedByRouter: false),
        };
        var vm = new AdminNestViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.FrontedByRouter);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task Load_RouterFronted_RendersServingPortReadOnly()
    {
        // A router-fronted Docker/cloud nest serves a fixed 443 (the serving_port
        // singleton is rejected there — fauna.node_policy.serving_port_fronted), so
        // the admin client renders the admin-nest-serving-port field read-only
        // (nest/common.md § Serving ports). The VM reads fronted_by_router off the
        // SAME fauna.setup.status fetch that seeds the port value.
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(443, frontedByRouter: true),
        };
        var vm = new AdminNestViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.FrontedByRouter);
        Assert.Null(vm.Error);
    }

    // ── Host-OS maintenance (installers/vps.md § Host OS Maintenance § 4) ──

    [Fact]
    public async Task Load_PopulatesOsMaintenance_UpToDate()
    {
        // A nest with no host maintenance channel (the default) reports the serde
        // defaults (0 / false) off fauna.setup.status → "OS up to date", neither the
        // count badge nor the restart button (vps.md § 4 — no false alarm on skew).
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
        };
        var vm = new AdminNestViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal((uint)0, vm.OsSecurityUpdatesPending);
        Assert.False(vm.OsRebootPending);
        // No localizer in the test host → Resolve falls back to the dotted key.
        Assert.Equal("admin.nest_page.os_up_to_date", vm.OsMaintenanceStatus);
        Assert.Null(vm.Error);
        Assert.Contains("SetupStatus", rpc.Calls);
    }

    [Fact]
    public async Task Load_PopulatesOsMaintenance_RestartPending()
    {
        // A host reporting a pending reboot + pending updates: the line is the
        // reboot headline, the count carries the raw integer, the button shows.
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(
                osSecurityUpdatesPending: 3, osRebootPending: true),
        };
        var vm = new AdminNestViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal((uint)3, vm.OsSecurityUpdatesPending);
        Assert.True(vm.OsRebootPending);
        Assert.Equal("admin.nest_page.os_restart_pending", vm.OsMaintenanceStatus);
        Assert.Null(vm.Error);
    }

    [Theory]
    [InlineData(0u, false, "admin.nest_page.os_up_to_date")]
    [InlineData(5u, false, "admin.nest_page.os_updates_pending")]
    [InlineData(0u, true, "admin.nest_page.os_restart_pending")]
    [InlineData(3u, true, "admin.nest_page.os_restart_pending")] // reboot is the headline
    public void OsMaintenanceStatusLine_ResolvesSharedKey(
        uint updates, bool reboot, string expectedKey)
    {
        // The state→key decision is single-sourced in shared Rust
        // (os_maintenance_status_label); the windows VM is a pure consume (no per-app
        // map). The test host has no localizer, so Resolve falls back to the dotted key.
        Assert.Equal(expectedKey, AdminNestViewModel.OsMaintenanceStatusLine(updates, reboot));
    }

    [Fact]
    public async Task RestartNow_RequestsHostRestart_ThenRereads()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(
                osSecurityUpdatesPending: 3, osRebootPending: true),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        // After the request the coordinator will reboot; model the cleared host-status.
        rpc.NextSetupStatus = MockNestRpcClient.MakeSetupStatus();
        await vm.RestartNowAsync();

        // The "restart now" rode fauna.admin.request_host_restart, followed by a
        // setup.status re-read (reflective, not optimistic — mirrors the serving-port save).
        Assert.True(rpc.HostRestartRequested);
        var calls = rpc.Calls.ToList();
        int req = calls.LastIndexOf("RequestHostRestart");
        int status = calls.LastIndexOf("SetupStatus");
        Assert.True(req >= 0 && status > req,
            $"expected a setup.status re-read after request_host_restart; calls=[{string.Join(",", calls)}]");
        // The re-read reflected the cleared state.
        Assert.False(vm.OsRebootPending);
        Assert.Equal("admin.nest_page.os_up_to_date", vm.OsMaintenanceStatus);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task RestartNow_Failure_RoutesToPageError()
    {
        // A nest with no maintenance mount rejects request_host_restart
        // (fauna.host_maintenance.no_host); the failure routes to the page-level
        // error-message surface, not an unhandled throw (vps.md § 4).
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(
                osRebootPending: true),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        rpc.NextError = "no_host";
        await vm.RestartNowAsync();

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }

    // ── Declared region (fauna.admin.region.{get,set}, admin.md § N Nest →
    //    Declared region) — every rendering decision is the shared
    //    admin_region_view fold; these tests pin the VM's plumbing only
    //    (fold + refetch + error routing), never re-derive the fold's own
    //    judgment. ──

    [Fact]
    public async Task Load_PopulatesUndeclaredRegion()
    {
        // The fold's own default (AdminRegionView::default()) — a NORMAL state
        // with words, never blank.
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
        };
        var vm = new AdminNestViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Null(vm.RegionDeclared);
        Assert.Equal("admin.nest_page.region_none", vm.RegionStatus);
        Assert.Null(vm.RegionAuthority);
        Assert.Null(vm.RegionStaleness);
        Assert.False(vm.RegionCanWithdraw);
        Assert.Contains("AdminRegionStatus", rpc.Calls);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task Load_PopulatesDeclaredRegion_WithAuthorityAndStaleness()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
            NextRegionView = new uniffi.fauna_ffi.FfiAdminRegionView(
                declared: "NO",
                status: new uniffi.fauna_core.LocalizedText(
                    "admin.nest_page.region_declared", new()),
                authority: new uniffi.fauna_core.LocalizedText(
                    "admin.nest_page.region_document", new()),
                staleness: new uniffi.fauna_core.LocalizedText(
                    "admin.nest_page.region_stale", new()),
                canWithdraw: true),
        };
        var vm = new AdminNestViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("NO", vm.RegionDeclared);
        Assert.Equal("admin.nest_page.region_declared", vm.RegionStatus);
        Assert.Equal("admin.nest_page.region_document", vm.RegionAuthority);
        Assert.Equal("admin.nest_page.region_stale", vm.RegionStaleness);
        Assert.True(vm.RegionCanWithdraw);
    }

    [Fact]
    public async Task SetRegion_DeclareSendsCodeThenRefetches()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetRegionAsync("NO");

        Assert.Equal(new string?[] { "NO" }, rpc.RegionSetCalls);
        // set is followed by a region.get refetch (reflective, not optimistic —
        // matches SetPairingAsync/SaveServingPortAsync).
        var calls = rpc.Calls.ToList();
        int set = calls.LastIndexOf("SetRegion");
        int get = calls.LastIndexOf("AdminRegionStatus");
        Assert.True(set >= 0 && get > set,
            $"expected a region refetch after set; calls=[{string.Join(",", calls)}]");
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task SetRegion_WithdrawSendsNullRegion()
    {
        // Withdrawing rides the SAME fauna.admin.region.set kind with the region
        // absent — the absent case, not a sentinel.
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetRegionAsync(null);

        Assert.Equal(new string?[] { null }, rpc.RegionSetCalls);
    }

    [Fact]
    public async Task SetRegion_FailureRoutesToPageError()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new uniffi.fauna_ffi.FfiAdminServiceFlags(false, false),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        rpc.NextError = "boom";
        await vm.SetRegionAsync("NO");

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }

    // ── Deployment-identity rotation (box-recovery.md § Deployment-seed
    // rotation) — the arm/roster/confirm/cancel ceremony, over MockNestRpcClient. ──

    [Fact]
    public async Task ArmSeedRotate_ResolvesToReadyWithRoster()
    {
        var rpc = new MockNestRpcClient
        {
            NextSeedRotateRoster = new uniffi.fauna_ffi.FfiSeedRotationConfirmView(
                new[] { new uniffi.fauna_ffi.FfiSeedRotationInheritor(new byte[32], "Alice") },
                canConfirm: true, blockedReason: null),
        };
        var vm = new AdminNestViewModel(rpc);

        await vm.ArmSeedRotateAsync();

        Assert.Equal(AdminNestViewModel.SeedRotateArmStage.Ready, vm.SeedRotateStage);
        Assert.True(vm.SeedRotateCanConfirm);
        Assert.Null(vm.SeedRotateReasonText);
        Assert.Single(vm.SeedRotateRoster);
        Assert.Equal("Alice", vm.SeedRotateRoster[0].Label);
        Assert.Equal(1, rpc.SeedRotateRosterCalls);
    }

    [Fact]
    public async Task ArmSeedRotate_EmptyRosterIsSelfRefutingAndBlocksConfirm()
    {
        // box-recovery.md's self-refuting case: the admin looking at this screen
        // IS an admin, so a roster of nobody is a wrong answer — the shared fold
        // sets can_confirm=false + a blocked_reason rather than an empty-but-live
        // confirm.
        var rpc = new MockNestRpcClient
        {
            NextSeedRotateRoster = new uniffi.fauna_ffi.FfiSeedRotationConfirmView(
                System.Array.Empty<uniffi.fauna_ffi.FfiSeedRotationInheritor>(),
                canConfirm: false,
                blockedReason: new uniffi.fauna_core.LocalizedText("admin.nest_page.rotate_seed_roster_empty", new())),
        };
        var vm = new AdminNestViewModel(rpc);

        await vm.ArmSeedRotateAsync();

        Assert.Equal(AdminNestViewModel.SeedRotateArmStage.Ready, vm.SeedRotateStage);
        Assert.False(vm.SeedRotateCanConfirm);
        Assert.False(string.IsNullOrEmpty(vm.SeedRotateReasonText));
        Assert.Empty(vm.SeedRotateRoster);
    }

    [Fact]
    public async Task ArmSeedRotate_Failure_RoutesToFailedStageNotPageError()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new AdminNestViewModel(rpc);

        await vm.ArmSeedRotateAsync();

        Assert.Equal(AdminNestViewModel.SeedRotateArmStage.Failed, vm.SeedRotateStage);
        Assert.False(string.IsNullOrEmpty(vm.SeedRotateReasonText));
        Assert.False(vm.SeedRotateCanConfirm);
        Assert.Empty(vm.SeedRotateRoster);
        // A failed roster read is not a page-level error — it's withheld reason
        // text inside the (still-visible, disabled) confirm surface.
        Assert.Null(vm.Error);
    }

    [Fact]
    public void CancelSeedRotate_DropsBackToUnarmed()
    {
        var rpc = new MockNestRpcClient();
        var vm = new AdminNestViewModel(rpc);
        vm.SeedRotateStage = AdminNestViewModel.SeedRotateArmStage.Ready;
        vm.SeedRotateCanConfirm = true;
        vm.SeedRotateRoster.Add(new SeedRotateRosterRow { Label = "Alice" });

        vm.CancelSeedRotate();

        Assert.Equal(AdminNestViewModel.SeedRotateArmStage.None, vm.SeedRotateStage);
        Assert.False(vm.SeedRotateCanConfirm);
        Assert.Empty(vm.SeedRotateRoster);
        Assert.Null(vm.SeedRotateReasonText);
    }

    [Fact]
    public async Task ConfirmSeedRotate_DisarmsBeforeDispatchAndPaintsTheVerdict()
    {
        var rpc = new MockNestRpcClient
        {
            NextSeedRotationResult = new uniffi.fauna_ffi.FfiSeedRotationResult(
                rotated: true,
                verdict: new uniffi.fauna_core.LocalizedText("admin.nest_page.rotate_seed_done", new())),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.ArmSeedRotateAsync();
        Assert.Equal(AdminNestViewModel.SeedRotateArmStage.Ready, vm.SeedRotateStage);

        var confirmTask = vm.ConfirmSeedRotateAsync();
        // Disarmed SYNCHRONOUSLY, before the dispatch's await ever yields — a
        // double click on the same frame must not chain a second rotation.
        Assert.Equal(AdminNestViewModel.SeedRotateArmStage.None, vm.SeedRotateStage);
        Assert.Empty(vm.SeedRotateRoster);
        await confirmTask;

        Assert.Equal(1, rpc.RotateDeploymentSeedCalls);
        Assert.False(string.IsNullOrEmpty(vm.SeedRotateStatus));
        Assert.NotEqual("boom", vm.SeedRotateStatus);
    }

    [Fact]
    public async Task ConfirmSeedRotate_RefusedIdentityMismatchPaintsTheVerdict()
    {
        var rpc = new MockNestRpcClient
        {
            NextSeedRotationResult = new uniffi.fauna_ffi.FfiSeedRotationResult(
                rotated: false,
                verdict: new uniffi.fauna_core.LocalizedText("admin.nest_page.rotate_seed_mismatch", new())),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.ArmSeedRotateAsync();

        await vm.ConfirmSeedRotateAsync();

        Assert.Equal(1, rpc.RotateDeploymentSeedCalls);
        Assert.False(string.IsNullOrEmpty(vm.SeedRotateStatus));
    }

    [Fact]
    public async Task ConfirmSeedRotate_WithoutArmingIsANoOp()
    {
        var rpc = new MockNestRpcClient();
        var vm = new AdminNestViewModel(rpc);

        await vm.ConfirmSeedRotateAsync();

        Assert.Equal(0, rpc.RotateDeploymentSeedCalls);
        Assert.Null(vm.SeedRotateStatus);
    }

    [Fact]
    public async Task ConfirmSeedRotate_Failure_RoutesToStatusNotPageError()
    {
        var rpc = new MockNestRpcClient
        {
            NextSeedRotateRoster = new uniffi.fauna_ffi.FfiSeedRotationConfirmView(
                new[] { new uniffi.fauna_ffi.FfiSeedRotationInheritor(new byte[32], "Alice") },
                canConfirm: true, blockedReason: null),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.ArmSeedRotateAsync();

        rpc.NextError = "boom";
        await vm.ConfirmSeedRotateAsync();

        Assert.False(string.IsNullOrEmpty(vm.SeedRotateStatus));
        Assert.Null(vm.Error);
    }

    // ── Outside-app sign-in keys (admin-nest-oauth-*, authorization-server.md
    //    § The issuer → Two rotation arms) ─────────────────────────────────────

    [Fact]
    public async Task Load_PopulatesOauthKeyRowsAndRotateCost()
    {
        var rpc = new MockNestRpcClient();
        var vm = new AdminNestViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Null(vm.Error);
        Assert.Single(vm.OauthKeyRows);
        Assert.Null(vm.OauthKeyReason);
        Assert.False(string.IsNullOrEmpty(vm.OauthRotateCost));
        Assert.True(vm.OauthControlsLive);
        Assert.Contains("AdminIssuerKeyStatus", rpc.Calls);
    }

    [Fact]
    public async Task Load_OauthReadFailure_RoutesToKeyReasonNeverPageError()
    {
        // Non-fatal to the rest of the page (mirrors tui's read_oauth_keys):
        // a failed key-set read is the section's OWN reason line, never
        // LoadAsync's Error — every other section must still paint.
        var rpc = new MockNestRpcClient { AdminIssuerKeyStatusThrows = true };
        var vm = new AdminNestViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Null(vm.Error);
        Assert.Empty(vm.OauthKeyRows);
        Assert.Null(vm.OauthRotateCost);
        Assert.False(string.IsNullOrEmpty(vm.OauthKeyReason));
        Assert.False(vm.OauthControlsLive);
        // The rest of the page still hydrated.
        Assert.Contains("SetupStatus", rpc.Calls);
    }

    [Fact]
    public async Task RotateIssuerKey_RefusesWhileTheKeySetHasNotAnswered()
    {
        var rpc = new MockNestRpcClient { AdminIssuerKeyStatusThrows = true };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.False(vm.OauthControlsLive);

        await vm.RotateIssuerKeyAsync();

        Assert.Equal(0, rpc.AdminRotateIssuerKeyCalls);
        Assert.Null(vm.OauthStatus);
    }

    [Fact]
    public async Task RotateIssuerKey_DispatchesAndReReadsInOneStateUpdate()
    {
        var rpc = new MockNestRpcClient
        {
            NextOauthVerdict = new uniffi.fauna_core.LocalizedText(
                "admin.nest_page.oauth_rotate_done", new() { ["kid"] = "kid-2" }),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.RotateIssuerKeyAsync();

        Assert.Equal(1, rpc.AdminRotateIssuerKeyCalls);
        // Re-read after the dispatch (the contract point the first drafts got
        // wrong): AdminIssuerKeyStatus is called once at Load and once again
        // after the rotation, so the rows reflect the post-rotation set, not
        // a verdict published ahead of it.
        Assert.Equal(2, rpc.AdminIssuerKeyStatusCalls);
        Assert.False(string.IsNullOrEmpty(vm.OauthStatus));
        Assert.False(vm.OauthInFlight);
        Assert.False(vm.OauthConfirmArmed);
    }

    [Fact]
    public async Task RotateIssuerKey_DisarmsAnyForcedConfirmFirst()
    {
        // A rotation about to change the key count would leave an armed
        // confirm stating a stale cost — the same disarm tui's
        // Action::RotateIssuerKey applies.
        var rpc = new MockNestRpcClient();
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.OpenOauthForcedConfirm(uniffi.fauna_ffi.FfiIssuerForcedArm.IssuerKey);
        Assert.True(vm.OauthConfirmArmed);

        await vm.RotateIssuerKeyAsync();

        Assert.False(vm.OauthConfirmArmed);
        Assert.Null(vm.OauthArmedArm);
    }

    [Fact]
    public async Task RotateIssuerKey_Failure_RoutesToStatusNotPageError()
    {
        var rpc = new MockNestRpcClient();
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        rpc.NextError = "boom";
        await vm.RotateIssuerKeyAsync();

        Assert.False(string.IsNullOrEmpty(vm.OauthStatus));
        Assert.Null(vm.Error);
        Assert.False(vm.OauthInFlight);
    }

    [Fact]
    public async Task OpenOauthForcedConfirm_CapturesTheCostAtArmTime()
    {
        var rpc = new MockNestRpcClient();
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.OpenOauthForcedConfirm(uniffi.fauna_ffi.FfiIssuerForcedArm.SessionSecret);

        Assert.True(vm.OauthConfirmArmed);
        Assert.Equal(uniffi.fauna_ffi.FfiIssuerForcedArm.SessionSecret, vm.OauthArmedArm);
        Assert.False(string.IsNullOrEmpty(vm.OauthConfirmSummary));
        Assert.False(string.IsNullOrEmpty(vm.OauthConfirmLabel));
        // Arming dispatches nothing over the wire.
        Assert.Equal(0, rpc.AdminRotateIssuerKeyCalls);
        Assert.Empty(rpc.ForceRotateCalls);
    }

    [Fact]
    public async Task OpenOauthForcedConfirm_RefusesWhileTheKeySetHasNotAnswered()
    {
        var rpc = new MockNestRpcClient { AdminIssuerKeyStatusThrows = true };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.OpenOauthForcedConfirm(uniffi.fauna_ffi.FfiIssuerForcedArm.IssuerKey);

        Assert.False(vm.OauthConfirmArmed);
        Assert.Null(vm.OauthArmedArm);
    }

    [Fact]
    public async Task CancelOauthForced_DropsBackToUnarmedTouchingNothing()
    {
        var rpc = new MockNestRpcClient();
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.OpenOauthForcedConfirm(uniffi.fauna_ffi.FfiIssuerForcedArm.IssuerKey);

        vm.CancelOauthForced();

        Assert.False(vm.OauthConfirmArmed);
        Assert.Null(vm.OauthArmedArm);
        Assert.Null(vm.OauthConfirmSummary);
        Assert.Null(vm.OauthConfirmLabel);
        Assert.Empty(rpc.ForceRotateCalls);
    }

    [Fact]
    public async Task ConfirmOauthForced_DisarmsBeforeDispatchAndFiresOnlyTheArmedArm()
    {
        var rpc = new MockNestRpcClient();
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.OpenOauthForcedConfirm(uniffi.fauna_ffi.FfiIssuerForcedArm.IssuerKey);

        var ceremony = vm.ConfirmOauthForcedAsync(uniffi.fauna_ffi.FfiIssuerForcedArm.IssuerKey);
        // Disarmed SYNCHRONOUSLY, before the dispatch's await ever yields —
        // a double press must not dispatch a second forced rotation.
        Assert.False(vm.OauthConfirmArmed);
        Assert.Null(vm.OauthArmedArm);
        await ceremony;

        Assert.Equal(
            new[] { uniffi.fauna_ffi.FfiIssuerForcedArm.IssuerKey }, rpc.ForceRotateCalls);
        // Re-read after the dispatch, same one-state-update contract as the
        // ordinary rotation.
        Assert.Equal(2, rpc.AdminIssuerKeyStatusCalls);
        Assert.False(string.IsNullOrEmpty(vm.OauthStatus));
    }

    [Fact]
    public async Task ConfirmOauthForced_MismatchedArmDispatchesNothing()
    {
        // A confirm painted for one arm, pressed after the other was armed:
        // dispatch nothing, and keep what the admin can see (mirrors tui's
        // Action::ConfirmOauthForced mismatch guard).
        var rpc = new MockNestRpcClient();
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.OpenOauthForcedConfirm(uniffi.fauna_ffi.FfiIssuerForcedArm.SessionSecret);

        await vm.ConfirmOauthForcedAsync(uniffi.fauna_ffi.FfiIssuerForcedArm.IssuerKey);

        Assert.Empty(rpc.ForceRotateCalls);
        // The armed confirm survives the mismatched press untouched.
        Assert.True(vm.OauthConfirmArmed);
        Assert.Equal(uniffi.fauna_ffi.FfiIssuerForcedArm.SessionSecret, vm.OauthArmedArm);
    }

    [Fact]
    public async Task ConfirmOauthForced_Failure_RoutesToStatusNotPageError()
    {
        var rpc = new MockNestRpcClient();
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.OpenOauthForcedConfirm(uniffi.fauna_ffi.FfiIssuerForcedArm.IssuerKey);

        rpc.NextError = "boom";
        await vm.ConfirmOauthForcedAsync(uniffi.fauna_ffi.FfiIssuerForcedArm.IssuerKey);

        Assert.False(string.IsNullOrEmpty(vm.OauthStatus));
        Assert.Null(vm.Error);
        Assert.False(vm.OauthInFlight);
    }
}
