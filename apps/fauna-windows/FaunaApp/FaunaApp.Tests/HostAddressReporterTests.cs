using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Unit tests for <see cref="HostAddressReporter"/> — the on-connect admin
/// host-address report that makes ACME HTTP-01 gate on the STRONG resolve-check
/// (<c>domains-and-tls-bootstrap.md</c> § Host-address acquisition). The
/// load-bearing contract is the <c>am_i_admin</c> gate: only the admin reports
/// (a non-admin's <c>set_host_address</c> is refused nest-side, so an ungated
/// call is a pointless failing RPC every connect). Best-effort — a fault must
/// never surface into login (the reconnect/knock-pump stance).
/// </summary>
public class HostAddressReporterTests
{
    private static (MockNestRpcClient rpc, HostAddressReporter reporter) Build()
    {
        var rpc = new MockNestRpcClient();
        return (rpc, new HostAddressReporter(rpc));
    }

    [Fact]
    public async Task Run_WhenAdmin_ChecksAdminThenReports()
    {
        var (rpc, reporter) = Build();
        rpc.NextAmIAdmin = true;
        rpc.NextHostAddressOutcome = new FfiHostAddressOutcome.Reported("203.0.113.7");

        await reporter.RunAsync();

        // Gate is consulted FIRST, then the report fires — the exact linux shape
        // (`if is_admin { report_host_address() }`).
        Assert.Equal(new[] { "AmIAdmin", "ReportHostAddress" }, rpc.Calls.ToArray());
    }

    [Fact]
    public async Task Run_WhenNotAdmin_ChecksAdminButDoesNotReport()
    {
        var (rpc, reporter) = Build();
        rpc.NextAmIAdmin = false;

        await reporter.RunAsync();

        // The gate MUST be consulted, and a non-admin MUST NOT fire the report
        // (else every non-admin connect makes a pointless refused RPC).
        Assert.Contains("AmIAdmin", rpc.Calls);
        Assert.DoesNotContain("ReportHostAddress", rpc.Calls);
    }

    [Fact]
    public async Task Run_WhenReportFails_IsBestEffort_DoesNotThrow()
    {
        var (rpc, reporter) = Build();
        rpc.NextAmIAdmin = true;
        rpc.NextReportHostAddressError = "set_host_address transport fault";

        await reporter.RunAsync(); // must not throw

        // The report was attempted (the admin gate passed) and the fault swallowed.
        Assert.Contains("ReportHostAddress", rpc.Calls);
    }
}
