using System;
using System.IO;
using System.Linq;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using uniffi.fauna_log;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Conformance for the shared <see cref="ViewModelBase"/> error funnel: a VM
/// <c>SetError</c>/<c>ShowError</c> must (1) populate the bound <c>ErrorMessage</c>
/// (the page <c>ErrorBar</c> InfoBar still paints, unchanged) AND (2) record the
/// displayed message in the <c>fauna_log</c> ring at the producer, once per error
/// transition (observability.md § What must be logged cat. 1, § Log on the *event*
/// not the *paint*). Clearing the error logs nothing (don't log noise). Real-FFI
/// ring; assert PRESENCE of a unique message (process-global, racy).
/// </summary>
public class ViewModelBaseTests
{
    private sealed class TestVm : ViewModelBase
    {
        public void RaiseError(string? m) => SetError(m);
        public void RaiseError(Exception ex) => ShowError(ex);
    }

    private static void EnsureRingInstalled()
    {
        try { FaunaFfiMethods.InstallLogging(Path.Combine(Path.GetTempPath(), "fauna-vmbase-test")); }
        catch { /* already installed this run */ }
    }

    [Fact]
    public void SetError_AssignsErrorMessage_AndReachesTheRing()
    {
        EnsureRingInstalled();
        var vm = new TestVm();
        var msg = "vmbase-set-" + Guid.NewGuid();

        vm.RaiseError(msg);

        Assert.Equal(msg, vm.ErrorMessage);
        var hit = FaunaFfiMethods.LogSnapshot().FirstOrDefault(e => e.message == msg);
        Assert.NotNull(hit);
        Assert.Equal(LogLevel.Error, hit!.level);
        Assert.Equal("fauna_windows::TestVm", hit.target);
    }

    [Fact]
    public void SetError_Null_ClearsWithoutLogging()
    {
        EnsureRingInstalled();
        var vm = new TestVm();
        var sentinel = "vmbase-clear-" + Guid.NewGuid();

        vm.RaiseError(sentinel);   // one TestVm-targeted entry (non-empty)
        vm.RaiseError((string?)null); // a clear must add NO entry

        Assert.Null(vm.ErrorMessage);
        // A logged clear would surface as a TestVm-targeted entry with an empty
        // message — robust against parallel tests (no other test logs an empty msg).
        Assert.DoesNotContain(
            FaunaFfiMethods.LogSnapshot(),
            e => e.target == "fauna_windows::TestVm" && string.IsNullOrEmpty(e.message));
    }

    [Fact]
    public void ShowError_LogsTheDisplayedString()
    {
        EnsureRingInstalled();
        var vm = new TestVm();
        var ex = new InvalidOperationException("vmbase-ex-" + Guid.NewGuid());

        vm.RaiseError(ex);

        // ShowError displays Strings.Error(ex); the ring records the SAME displayed
        // string (the funnel logs the message it shows, redaction-safe by construction).
        Assert.NotNull(vm.ErrorMessage);
        Assert.Contains(
            FaunaFfiMethods.LogSnapshot(),
            e => e.message == vm.ErrorMessage && e.level == LogLevel.Error
                 && e.target == "fauna_windows::TestVm");
    }
}
