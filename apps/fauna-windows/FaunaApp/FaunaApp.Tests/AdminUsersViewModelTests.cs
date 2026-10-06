using Xunit;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the consolidated <c>admin-users</c> hub VM, over
/// the <see cref="MockNestRpcClient"/> WS-RPC seam (admin.md § Users; the e2e
/// flows in test_admin_users_hub.py, made deterministic — no live nest / FlaUI).
/// </summary>
public class AdminUsersViewModelTests
{
    // 32-byte actor ids as hex (64 chars).
    private static readonly string AdminActor = "aa" + new string('0', 62);
    private static readonly string JoinerActor = "bb" + new string('0', 62);

    private static MockNestRpcClient SeededRpc() => new()
    {
        NextAdminUsers = new[]
        {
            MockNestRpcClient.MakeAdminUser(AdminActor, tier: "free", label: "admin"),
        },
        NextAdminUsersTotal = 1,
        NextAdminTiers = new[]
        {
            MockNestRpcClient.MakeAdminTier("free"),
            MockNestRpcClient.MakeAdminTier("personal"),
            MockNestRpcClient.MakeAdminTier("community"),
        },
        NextAdminInviteCodes = new[]
        {
            MockNestRpcClient.MakeAdminInviteCode("EXISTING", "free", 2),
        },
        NextAdminInviteRequests = new[]
        {
            MockNestRpcClient.MakeAdminInviteRequest(7, handle: "joiner", actorIdHex: JoinerActor),
        },
    };

    [Fact]
    public async Task Load_PopulatesAllThreeSectionsAndTiers()
    {
        var vm = new AdminUsersViewModel(SeededRpc());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Single(vm.Users);
        Assert.Equal(1, vm.UserCount);
        Assert.Equal(AdminActor, vm.Users[0].ActorIdHex);
        Assert.Equal("free", vm.Users[0].Tier);
        // Read-only serving audit flag (AdminUser.mail_serving_enabled) defaults on.
        Assert.True(vm.Users[0].MailServingEnabled);
        Assert.Single(vm.InviteCodes);
        Assert.Equal("EXISTING", vm.InviteCodes[0].Code);
        Assert.Single(vm.InviteRequests);
        Assert.Equal("joiner", vm.InviteRequests[0].Handle);
        Assert.Equal(new[] { "free", "personal", "community" }, vm.Tiers);
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task Load_RowsCarryProjectedMailServingFlag()
    {
        // The read-only serving indicator must render the *projected* per-user
        // flag (AdminUser.mail_serving_enabled), not a constant — so a serving
        // and a non-serving user map to different row values (admin.md § Users).
        var rpc = SeededRpc();
        rpc.NextAdminUsers = new[]
        {
            MockNestRpcClient.MakeAdminUser(AdminActor, "free", "admin", mailServingEnabled: true),
            MockNestRpcClient.MakeAdminUser(JoinerActor, "free", "joiner", mailServingEnabled: false),
        };
        rpc.NextAdminUsersTotal = 2;
        var vm = new AdminUsersViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.Users[0].MailServingEnabled);
        Assert.False(vm.Users[1].MailServingEnabled);
    }

    [Fact]
    public async Task Load_RowsCarryHandleElseHexPickerOption_NeverTheRawLabel()
    {
        // Guardian (and other admin) pickers must identify a user by handle,
        // falling back to the full actor hex for a handle-less account -- the
        // editable, non-unique label is never a picker identity (admin.md § 2
        // Users -- what identifies a user in an admin picker). Two accounts
        // sharing the same label must still render two distinct options.
        var rpc = SeededRpc();
        rpc.NextAdminUsers = new[]
        {
            MockNestRpcClient.MakeAdminUser(AdminActor, "free", label: "Alex", handle: "alex"),
            MockNestRpcClient.MakeAdminUser(JoinerActor, "free", label: "Alex", handle: null),
        };
        rpc.NextAdminUsersTotal = 2;
        var vm = new AdminUsersViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("alex", vm.Users[0].PickerOption);
        Assert.NotEqual(vm.Users[0].PickerOption, vm.Users[1].PickerOption);
        Assert.Contains(JoinerActor, vm.Users[1].PickerOption, StringComparison.OrdinalIgnoreCase);
    }

    // ── Guardian candidates (admin.md § 2 → *Which accounts a picker offers*) — both admission guardian pickers offer every
    //    account on the nest, read separately from the paginated Users page. ──

    [Fact]
    public async Task Load_PopulatesGuardianCandidatesFromEveryAccountOnTheNest()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Single(vm.GuardianCandidates);
        Assert.Equal(AdminActor, vm.GuardianCandidates[0].ActorIdHex);
        Assert.Contains("AdminUsersListAll", rpc.Calls);
    }

    [Fact]
    public async Task ChangeUserTier_AlsoRefetchesGuardianCandidates()
    {
        // A users-page refetch keeps guardian eligibility current too (mirrors linux's
        // paired AdminUsersLoaded{reply,picker_users}), not just the initial page load.
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.ChangeUserTierAsync(vm.Users[0], "personal");

        var calls = rpc.Calls.ToList();
        int upd = calls.LastIndexOf("AdminUsersUpdate");
        int listAll = calls.LastIndexOf("AdminUsersListAll");
        Assert.True(upd >= 0 && listAll > upd,
            $"expected a guardian-candidates refetch after update; calls=[{string.Join(",", calls)}]");
    }

    [Fact]
    public async Task Load_FiltersOutNonPendingRequests()
    {
        var rpc = SeededRpc();
        rpc.NextAdminInviteRequests = new[]
        {
            MockNestRpcClient.MakeAdminInviteRequest(7, "joiner", JoinerActor, status: "pending"),
            MockNestRpcClient.MakeAdminInviteRequest(8, "denied-one", JoinerActor, status: "denied"),
        };
        var vm = new AdminUsersViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Single(vm.InviteRequests);
        Assert.Equal("joiner", vm.InviteRequests[0].Handle);
    }

    [Fact]
    public async Task ChangeUserTier_CallsUpdateWithActorAndTier_ThenRefetches()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.ChangeUserTierAsync(vm.Users[0], "personal");

        Assert.NotNull(rpc.LastAdminUserUpdate);
        Assert.Equal(AdminActor, Convert.ToHexString(rpc.LastAdminUserUpdate!.Value.ActorId).ToLowerInvariant());
        Assert.Equal("personal", rpc.LastAdminUserUpdate!.Value.Tier);
        // update is followed by a users refetch (proves it doesn't optimistic-flip).
        var calls = rpc.Calls;
        int upd = calls.ToList().LastIndexOf("AdminUsersUpdate");
        int list = calls.ToList().LastIndexOf("AdminUsersList");
        Assert.True(upd >= 0 && list > upd, $"expected a users refetch after update; calls=[{string.Join(",", calls)}]");
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task CreateCode_MintsOnEmpty_SurfacesCopyableToken()
    {
        var rpc = SeededRpc();
        rpc.NextMintedInviteCode = "MINTED-XYZ";
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.NewCodeTier = "personal";
        vm.NewCodeUses = 3;
        await vm.CreateCodeCommand.ExecuteAsync(null);

        // Mint-on-empty: the create rides an EMPTY code so the nest mints (admin.md § 3).
        Assert.NotNull(rpc.LastAdminInviteCreate);
        Assert.Equal("", rpc.LastAdminInviteCreate!.Value.Code);
        Assert.Equal("personal", rpc.LastAdminInviteCreate!.Value.Tier);
        Assert.Equal(3, rpc.LastAdminInviteCreate!.Value.Uses);
        Assert.Equal("MINTED-XYZ", vm.MintedCode);
        Assert.True(vm.ShowMintedCode);
        Assert.False(vm.IsCreatingCode);
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task DeleteCode_CallsDelete()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.DeleteCodeAsync("EXISTING");

        Assert.Contains("AdminInviteCodesDelete", rpc.Calls);
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task ApproveRequest_ApprovesAtChosenTier_ThenRefetchesBothSections()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.ApproveRequestAsync(vm.InviteRequests[0], "personal");

        Assert.NotNull(rpc.LastAdminApprove);
        Assert.Equal(7, rpc.LastAdminApprove!.Value.Id);
        Assert.Equal("personal", rpc.LastAdminApprove!.Value.Tier);
        Assert.Contains("AdminInviteRequestsApprove", rpc.Calls);
        Assert.Contains("AdminInviteRequestsList", rpc.Calls); // requests refetch
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task DenyRequest_CallsDenyWithReason()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.DenyRequestAsync(vm.InviteRequests[0], "spam");

        Assert.Contains("AdminInviteRequestsDeny", rpc.Calls);
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task ActionFailure_RoutesToActionError()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        rpc.NextError = "boom";
        await vm.ChangeUserTierAsync(vm.Users[0], "personal");

        // Failures route to the dedicated action-error surface (not the app banner
        // and not an unhandled throw). The message is formatted via i18n
        // (Strings.Error → "errors/http_error"); in the test host strings aren't
        // loaded, so assert the surface is populated rather than the exact text.
        Assert.False(string.IsNullOrEmpty(vm.ActionError));
    }

    // ── Eviction (admin.md § Users — one-click default reason + "other"; the user
    //    is NOT deleted, the row flips to the cancel control on refetch) ──

    [Fact]
    public async Task EvictUser_SendsDefaultReasonAndOtherCategory_ThenRefetches()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.EvictUserAsync(vm.Users[0]);

        Assert.NotNull(rpc.LastAdminUserEvict);
        Assert.Equal(AdminActor, Convert.ToHexString(rpc.LastAdminUserEvict!.Value.ActorId).ToLowerInvariant());
        // One-click: a default reason (from the evict_default_reason i18n string —
        // in the test host strings aren't loaded so it falls back to the key) + the
        // "other" category are the only inputs the row exposes (matches linux).
        Assert.Equal("other", rpc.LastAdminUserEvict!.Value.Category);
        Assert.Equal("admin/users_page/evict_default_reason", rpc.LastAdminUserEvict!.Value.Reason);
        // Evict is followed by a users refetch (the row flip is proven by the
        // round-trip, not an optimistic toggle).
        var calls = rpc.Calls.ToList();
        int evict = calls.LastIndexOf("AdminUsersEvict");
        int list = calls.LastIndexOf("AdminUsersList");
        Assert.True(evict >= 0 && list > evict, $"expected a users refetch after evict; calls=[{string.Join(",", calls)}]");
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task SuspendUser_SendsDefaultReasonAndOtherCategory_ThenRefetches()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SuspendUserAsync(vm.Users[0]);

        Assert.NotNull(rpc.LastAdminUserSuspend);
        Assert.Equal(AdminActor, Convert.ToHexString(rpc.LastAdminUserSuspend!.Value.ActorId).ToLowerInvariant());
        Assert.Equal("other", rpc.LastAdminUserSuspend!.Value.Category);
        Assert.Equal("admin/users_page/suspend_default_reason", rpc.LastAdminUserSuspend!.Value.Reason);
        var calls = rpc.Calls.ToList();
        int suspend = calls.LastIndexOf("AdminUsersSuspend");
        int list = calls.LastIndexOf("AdminUsersList");
        Assert.True(suspend >= 0 && list > suspend, $"expected a users refetch after suspend; calls=[{string.Join(",", calls)}]");
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task SuspendFailure_RoutesToActionError()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        rpc.NextError = "boom";
        await vm.SuspendUserAsync(vm.Users[0]);

        Assert.False(string.IsNullOrEmpty(vm.ActionError));
    }

    // ── Row lifecycle controls (admin.md § 2 Users → Cutting a user off) —
    //    the shared fauna_client_admin::admin_user_row_controls decision, not
    //    re-derived client-side. ──

    [Fact]
    public async Task ActiveRow_OffersSuspendAndEvict_NotRestore()
    {
        var rpc = SeededRpc();
        rpc.NextAdminUsers = new[] { MockNestRpcClient.MakeAdminUser(AdminActor, "free", "joiner") };
        rpc.NextAdminUsersTotal = 1;
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.Users[0].CanSuspend);
        Assert.True(vm.Users[0].CanEvict);
        Assert.False(vm.Users[0].EvictionActive);
    }

    [Fact]
    public async Task WarningRow_StillOffersSuspend_ButNotEvict()
    {
        // Suspend stays reachable from a mid-eviction `warning` row (it clears the
        // pending delete, erring away from deletion) — the transition the old
        // windows CanEvict = !EvictionActive shortcut made unreachable elsewhere.
        var rpc = SeededRpc();
        rpc.NextAdminUsers = new[]
        {
            MockNestRpcClient.MakeAdminUser(AdminActor, "free", "joiner",
                eviction: MockNestRpcClient.MakeAdminEviction(status: "warning")),
        };
        rpc.NextAdminUsersTotal = 1;
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.Users[0].CanSuspend);
        Assert.False(vm.Users[0].CanEvict);
        Assert.True(vm.Users[0].EvictionActive);
    }

    [Fact]
    public async Task AdminRow_OffersNeitherEntryControl_ButKeepsRestore()
    {
        // An admin can be neither suspended nor evicted (fauna.admin.conflict) — the
        // row withholds both entry controls but restore must survive the admin guard
        // (granting admin to an already-suspended user must not strand them).
        var rpc = SeededRpc();
        rpc.NextAdminUsers = new[]
        {
            MockNestRpcClient.MakeAdminUser(AdminActor, "free", "admin",
                eviction: MockNestRpcClient.MakeAdminEviction(status: "suspended"),
                isAdmin: true),
        };
        rpc.NextAdminUsersTotal = 1;
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.Users[0].CanSuspend);
        Assert.False(vm.Users[0].CanEvict);
        Assert.True(vm.Users[0].EvictionActive);
    }

    [Fact]
    public async Task CancelEviction_CallsCancelWithActor_ThenRefetches()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.CancelEvictionAsync(vm.Users[0]);

        Assert.NotNull(rpc.LastAdminUserCancelEviction);
        Assert.Equal(AdminActor, Convert.ToHexString(rpc.LastAdminUserCancelEviction!).ToLowerInvariant());
        var calls = rpc.Calls.ToList();
        int cancel = calls.LastIndexOf("AdminUsersCancelEviction");
        int list = calls.LastIndexOf("AdminUsersList");
        Assert.True(cancel >= 0 && list > cancel, $"expected a users refetch after cancel; calls=[{string.Join(",", calls)}]");
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task Load_RowWithPendingEviction_HasEvictionActive()
    {
        // The XAML evict↔cancel row flip is driven off AdminUserRow.EvictionActive,
        // which is mapped from FfiAdminUser.eviction is not null.
        var rpc = SeededRpc();
        rpc.NextAdminUsers = new[]
        {
            MockNestRpcClient.MakeAdminUser(AdminActor, "free", "admin"),
            MockNestRpcClient.MakeAdminUser(JoinerActor, "free", "joiner",
                eviction: MockNestRpcClient.MakeAdminEviction()),
        };
        rpc.NextAdminUsersTotal = 2;
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.Users[0].EvictionActive);
        Assert.True(vm.Users[1].EvictionActive);
    }

    [Fact]
    public async Task EvictFailure_RoutesToActionError()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        rpc.NextError = "boom";
        await vm.EvictUserAsync(vm.Users[0]);

        Assert.False(string.IsNullOrEmpty(vm.ActionError));
    }

    // ── Pagination (admin.md § Users — page size 50 over users.list limit/offset;
    //    prev clamps at 0, next clamps at the last page) ──

    [Fact]
    public async Task ReloadUsers_RequestsCurrentPage_AndCapturesTotal()
    {
        var rpc = SeededRpc();
        rpc.NextAdminUsersTotal = 120;
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(0, vm.Offset);
        Assert.Equal(120, vm.TotalUsers);
        Assert.Equal(1, vm.CurrentPage);
        Assert.Equal(3, vm.TotalPages); // ceil(120 / 50)
        Assert.False(vm.HasPrevPage);
        Assert.True(vm.HasNextPage);
    }

    [Fact]
    public async Task NextPage_AdvancesByPageSize_AndRefetches()
    {
        var rpc = SeededRpc();
        rpc.NextAdminUsersTotal = 120;
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.NextPageAsync();

        Assert.Equal(50, vm.Offset);
        Assert.Equal(2, vm.CurrentPage);
        Assert.True(vm.HasPrevPage);
        Assert.True(vm.HasNextPage); // 50 + 50 = 100 < 120
        Assert.Equal(2, rpc.Calls.Count(c => c == "AdminUsersList")); // load + next
    }

    [Fact]
    public async Task NextPage_ClampsAtLastPage()
    {
        var rpc = SeededRpc();
        rpc.NextAdminUsersTotal = 120;
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.NextPageAsync(); // 50
        await vm.NextPageAsync(); // 100 (last page: 100 + 50 = 150 !< 120)
        Assert.Equal(100, vm.Offset);
        Assert.Equal(3, vm.CurrentPage);
        Assert.False(vm.HasNextPage);

        int listsBefore = rpc.Calls.Count(c => c == "AdminUsersList");
        await vm.NextPageAsync(); // no-op at last page
        Assert.Equal(100, vm.Offset);
        Assert.Equal(listsBefore, rpc.Calls.Count(c => c == "AdminUsersList"));
    }

    [Fact]
    public async Task PrevPage_DecrementsByPageSize_AndClampsAtZero()
    {
        var rpc = SeededRpc();
        rpc.NextAdminUsersTotal = 120;
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        await vm.NextPageAsync(); // 50

        await vm.PrevPageAsync(); // back to 0
        Assert.Equal(0, vm.Offset);
        Assert.False(vm.HasPrevPage);

        int listsBefore = rpc.Calls.Count(c => c == "AdminUsersList");
        await vm.PrevPageAsync(); // no-op at first page
        Assert.Equal(0, vm.Offset);
        Assert.Equal(listsBefore, rpc.Calls.Count(c => c == "AdminUsersList"));
    }

    [Fact]
    public async Task UserCount_ReflectsTotalAcrossPages_NotJustLoadedPage()
    {
        // The count text reads "{N} users total" — N is the grand total (matches
        // linux update_users_page rendering reply.total), not the current page length.
        var rpc = SeededRpc();
        rpc.NextAdminUsers = new[] { MockNestRpcClient.MakeAdminUser(AdminActor, "free", "admin") };
        rpc.NextAdminUsersTotal = 73;
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Single(vm.Users);   // only the loaded page's rows render
        Assert.Equal(73, vm.UserCount); // but the total reflects all pages
    }

    // ── Registration (admin.md § 2 Users → Section 2; public-mode.md § Registration
    //    Modes) — the posture read off fauna.setup.status, never coerced. ──

    [Fact]
    public async Task Load_ReadsRegistrationPostureFromSetupStatus_AndSeedsDrafts()
    {
        var rpc = SeededRpc();
        rpc.NextSetupStatus = MockNestRpcClient.MakeSetupStatus() with
        {
            registrationMode = "invite_required", maxFreeUsers = 10,
        };
        var vm = new AdminUsersViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("invite_required", vm.RegistrationMode);
        Assert.Equal(10ul, vm.MaxFreeUsers);
        Assert.Equal("invite_required", vm.RegistrationModeDraft);
        Assert.Equal("10", vm.MaxFreeUsersInput);
    }

    [Fact]
    public async Task Load_UnknownPosture_LeavesDraftBlank_NeverCoerced()
    {
        // A reply with no posture (registrationMode == null) or reporting a
        // mode this build doesn't recognize must render read-only — the draft
        // must NOT collapse to a guessed variant a Save could dispatch and
        // overwrite the nest's real posture with.
        var rpc = SeededRpc();
        rpc.NextSetupStatus = MockNestRpcClient.MakeSetupStatus() with { registrationMode = null };
        var vm = new AdminUsersViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Null(vm.RegistrationMode);
        Assert.Equal("", vm.RegistrationModeDraft);
    }

    [Fact]
    public async Task SaveRegistration_SendsModeAndParsedCeiling_ThenRefetches()
    {
        var rpc = SeededRpc();
        rpc.NextSetupStatus = MockNestRpcClient.MakeSetupStatus() with { registrationMode = "open" };
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.RegistrationModeDraft = "closed";
        vm.MaxFreeUsersInput = "25";

        await vm.SaveRegistrationCommand.ExecuteAsync(null);

        Assert.NotNull(rpc.LastRegistrationModeSet);
        Assert.Equal(uniffi.fauna_ffi.FfiRegistrationMode.Closed, rpc.LastRegistrationModeSet!.Value.mode);
        Assert.Equal(25ul, rpc.LastRegistrationModeSet!.Value.maxFreeUsers);
        // Save is followed by a posture re-load (proves it doesn't optimistic-flip).
        var calls = rpc.Calls.ToList();
        int setReg = calls.LastIndexOf("AdminSetRegistrationMode");
        int status = calls.LastIndexOf("SetupStatus");
        Assert.True(setReg >= 0 && status > setReg, $"expected a posture reload after save; calls=[{string.Join(",", calls)}]");
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task SaveRegistration_BlankCeiling_SendsNullMaxFreeUsers()
    {
        var rpc = SeededRpc();
        rpc.NextSetupStatus = MockNestRpcClient.MakeSetupStatus() with { registrationMode = "open" };
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.RegistrationModeDraft = "open";
        vm.MaxFreeUsersInput = "  ";

        await vm.SaveRegistrationCommand.ExecuteAsync(null);

        Assert.NotNull(rpc.LastRegistrationModeSet);
        Assert.Null(rpc.LastRegistrationModeSet!.Value.maxFreeUsers);
    }

    [Fact]
    public async Task SaveRegistration_NonNumericCeiling_IsLocalErrorNeverDispatched()
    {
        var rpc = SeededRpc();
        rpc.NextSetupStatus = MockNestRpcClient.MakeSetupStatus() with { registrationMode = "open" };
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.RegistrationModeDraft = "open";
        vm.MaxFreeUsersInput = "not-a-number";

        await vm.SaveRegistrationCommand.ExecuteAsync(null);

        Assert.Null(rpc.LastRegistrationModeSet);
        Assert.DoesNotContain("AdminSetRegistrationMode", rpc.Calls);
        Assert.False(string.IsNullOrEmpty(vm.ActionError));
    }

    // ── Admit (admin.md § 2 Users → Section 3; public-mode.md § Registration &
    //    Identity — the third account-creation path) ──

    [Fact]
    public async Task AdmitUser_SendsActorTierHandle_ThenRefetchesUsers()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.AdmitActorInput = JoinerActor;
        vm.AdmitHandleInput = "newjoiner";
        vm.AdmitTierDraft = "personal";

        await vm.AdmitUserCommand.ExecuteAsync(null);

        Assert.NotNull(rpc.LastUsersCreate);
        Assert.Equal(JoinerActor, Convert.ToHexString(rpc.LastUsersCreate!.Value.actorId).ToLowerInvariant());
        Assert.Equal("personal", rpc.LastUsersCreate!.Value.tier);
        Assert.Equal("newjoiner", rpc.LastUsersCreate!.Value.handle);
        var calls = rpc.Calls.ToList();
        int create = calls.LastIndexOf("AdminUsersCreate");
        int list = calls.LastIndexOf("AdminUsersList");
        Assert.True(create >= 0 && list > create, $"expected a users refetch after admit; calls=[{string.Join(",", calls)}]");
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task AdmitUser_BlankHandle_SendsNullHandle_DeliberateHandleLess()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.AdmitActorInput = JoinerActor;
        vm.AdmitHandleInput = "  ";
        vm.AdmitTierDraft = "free";

        await vm.AdmitUserCommand.ExecuteAsync(null);

        Assert.NotNull(rpc.LastUsersCreate);
        Assert.Null(rpc.LastUsersCreate!.Value.handle);
    }

    [Fact]
    public async Task AdmitUser_InvalidActorHex_IsLocalErrorNeverDispatched()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.AdmitActorInput = "not-64-hex-chars";
        vm.AdmitTierDraft = "free";

        await vm.AdmitUserCommand.ExecuteAsync(null);

        Assert.Null(rpc.LastUsersCreate);
        Assert.DoesNotContain("AdminUsersCreate", rpc.Calls);
        Assert.False(string.IsNullOrEmpty(vm.ActionError));
    }
}
