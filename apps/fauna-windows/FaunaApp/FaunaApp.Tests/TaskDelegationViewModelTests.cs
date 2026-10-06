using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Settings "Task delegation" sub-page VM (participants.md § Task delegation):
/// per-kind runner + assignment picker over
/// <c>INestRpcClient.TaskDelegation{List,SetAssignment}Async</c>. Every row's
/// <c>PinOptions</c> must render verbatim — this layer holds no delegation
/// policy (priority #2).
/// </summary>
public class TaskDelegationViewModelTests
{
    private static FfiTaskDelegationRow BackupUploadRow(
        FfiRunnerStatus runner, FfiPinOption assignment, FfiPinOption[]? pinOptions = null) =>
        new(
            "backup-upload",
            new uniffi.fauna_core.LocalizedText("task_delegation.kind_backup_upload", new Dictionary<string, string>()),
            runner,
            assignment,
            pinOptions ?? new FfiPinOption[] { new FfiPinOption.Automatic(), new FfiPinOption.ThisDevice() });

    [Fact]
    public async Task LoadAsync_PopulatesRows_FromLiveKinds()
    {
        var rpc = new MockNestRpcClient
        {
            NextTaskDelegationRows = new[]
            {
                BackupUploadRow(new FfiRunnerStatus.ThisDevice(), new FfiPinOption.Automatic()),
            },
        };
        var vm = new TaskDelegationViewModel(rpc, "aa11");

        await vm.LoadAsync();

        Assert.Contains("TaskDelegationList", rpc.Calls);
        Assert.Equal("aa11", rpc.LastTaskDelegationListDeviceId);
        Assert.Single(vm.Rows);
        Assert.Equal("backup-upload", vm.Rows[0].TaskKind);
        Assert.Equal(new FfiPinOption.Automatic(), vm.Rows[0].Assignment);
        Assert.Equal(2, vm.Rows[0].PinOptions.Count);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task LoadAsync_RunnerOther_DoesNotThrow_WithNoDeviceLabelSeam()
    {
        // BuildDevicesMachineAsync needs a live FfiNestClient the mock can't
        // supply (throws NotSupportedException) — DeviceLabelsAsync is
        // best-effort and must degrade to an empty map, not fail the load. No
        // IStringLocalizer is registered in this unit-test host, so RunnerText
        // resolves to the raw (unsubstituted) i18n key rather than a rendered
        // string — this test only pins the load-doesn't-throw contract.
        var rpc = new MockNestRpcClient
        {
            NextTaskDelegationRows = new[]
            {
                BackupUploadRow(
                    new FfiRunnerStatus.Other(new FfiParticipantRef.Device("bb22")),
                    new FfiPinOption.Automatic()),
            },
        };
        var vm = new TaskDelegationViewModel(rpc, "aa11");

        await vm.LoadAsync();

        Assert.Null(vm.ErrorMessage);
        Assert.Single(vm.Rows);
        Assert.False(string.IsNullOrEmpty(vm.Rows[0].RunnerText));
    }

    [Fact]
    public async Task SetAssignmentAsync_PersistsPin_AndReloads()
    {
        var rpc = new MockNestRpcClient
        {
            NextTaskDelegationRows = new[]
            {
                BackupUploadRow(new FfiRunnerStatus.ThisDevice(), new FfiPinOption.Automatic()),
            },
        };
        var vm = new TaskDelegationViewModel(rpc, "aa11");
        await vm.LoadAsync();

        rpc.NextTaskDelegationRows = new[]
        {
            BackupUploadRow(new FfiRunnerStatus.ThisDevice(), new FfiPinOption.ThisDevice()),
        };
        await vm.SetAssignmentAsync("backup-upload", new FfiPinOption.ThisDevice());

        Assert.Equal(("aa11", "backup-upload", (FfiPinOption)new FfiPinOption.ThisDevice()), rpc.LastTaskDelegationSetAssignment);
        Assert.Equal(2, rpc.Calls.Count(c => c == "TaskDelegationList"));
        Assert.Equal(new FfiPinOption.ThisDevice(), vm.Rows[0].Assignment);
    }

    [Fact]
    public async Task LoadAsync_Failure_RoutesToError()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new TaskDelegationViewModel(rpc, "aa11");

        await vm.LoadAsync();

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.Empty(vm.Rows);
    }
}
