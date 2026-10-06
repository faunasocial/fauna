using System;
using System.Runtime.InteropServices;
using System.Threading;
using Xunit;
using Xunit.Abstractions;

namespace FaunaApp.Tests;

/// <summary>
/// Measures — never assumes — the stack size available to a .NET thread of the
/// kind that polls a <c>fauna-ffi</c> async export's Rust future
/// (<c>docs/goal/architecture/apps/native-async-execution.md</c> § The execution
/// model: "C# (Windows) — on the .NET async continuation thread"; § Implementation
/// status today (b)).
///
/// <para><b>Unlike Swift's cooperative pool or Kotlin's coroutine dispatcher, .NET
/// has no dedicated small-stack async-executor pool.</b> Reading the generated
/// <c>_UniFFIAsync.PollFuture</c> (<c>FaunaApp.Core/Generated/uniffi/fauna_ffi.cs</c>):
/// its FIRST poll call happens SYNCHRONOUSLY on whatever thread calls the async
/// wrapper — a <c>Task</c> only truly suspends once the underlying Rust future
/// genuinely yields across an await boundary. A fully-synchronous chain (the
/// PQ-crypto hazard class § The second measured incident describes) never yields
/// at all, so it runs entirely on the CALLING thread's own stack — there is no
/// separate "foreign executor thread" to measure on this platform, only the
/// ordinary CLR thread that happened to call in. Every real call site in this app
/// calls from a plain CLR thread of the OS-linked default stack size — the WinUI
/// UI thread for VM-issued calls (this codebase's "no <c>ConfigureAwait(false)</c>"
/// convention keeps every await's continuation on it), or a .NET ThreadPool worker
/// for calls issued off it. This test measures that default directly, via
/// <c>GetCurrentThreadStackLimits</c> (a first-party, documented OS API — "retrieves
/// the boundaries of the stack that was allocated by the system for the current
/// thread") on a freshly created thread with NO explicit stack-size override — the
/// exact default every real call site gets.</para>
/// </summary>
public class ForeignExecutorStackSizeTests
{
    private readonly ITestOutputHelper _output;

    public ForeignExecutorStackSizeTests(ITestOutputHelper output) => _output = output;

    [DllImport("kernel32.dll")]
    private static extern void GetCurrentThreadStackLimits(out UIntPtr lowLimit, out UIntPtr highLimit);

    /// <summary>The PQ leaf's worst measured cost after the 2026-08-25 fix
    /// (<c>native-async-execution.md</c>'s table, <c>opt-level = 2</c> column) —
    /// the threshold this app's default thread stack must clear with real
    /// margin.</summary>
    private const ulong PqLeafWorstCaseBytes = 32 * 1024;

    [Fact]
    public void The_default_thread_stack_clears_the_pq_leaf_with_real_margin()
    {
        ulong measured = 0;
        var thread = new Thread(() =>
        {
            GetCurrentThreadStackLimits(out var low, out var high);
            measured = high.ToUInt64() - low.ToUInt64();
        }); // No maxStackSize override — the same OS-linked default every real
            // call site (the UI thread, a ThreadPool worker) gets.
        thread.Start();
        thread.Join();

        _output.WriteLine($"[stack-measure] default thread stack = {measured} bytes ({measured / 1024.0:F1} KB)");

        Assert.True(measured > 0, "GetCurrentThreadStackLimits reported an empty stack");
        // See native-async-execution.md § Implementation status today for the
        // number this test measured and the recorded verdict.
        Assert.True(
            measured >= PqLeafWorstCaseBytes * 10,
            $"default thread stack is only {measured} bytes ({measured / 1024.0:F1} KB) -- far " +
            $"closer to the {PqLeafWorstCaseBytes}-byte PQ leaf than expected; re-measure and " +
            "update native-async-execution.md before trusting this margin");
    }
}
