using System;
using System.Collections.Concurrent;
using System.Threading;

namespace FaunaApp.Tests;

/// <summary>
/// A stand-in for WinUI's <c>DispatcherQueueSynchronizationContext</c>: one
/// thread, one FIFO queue, and — the point of it — a way to occupy that thread
/// the way a single long UIA call does (the windows e2e bridge has logged 17 s
/// and 25 s holds during `test_bundled_provider.py --app windows`).
///
/// <para>Its job is to make thread-affinity bugs reproducible in a unit test.
/// Anything that only misbehaves when continuations must queue behind a busy
/// single-threaded context — a UniFFI poll loop, a ViewModel sequencing two
/// awaited machine calls — misbehaves here too, in seconds, with no WinUI, no
/// app launch and no UIA in the picture.</para>
///
/// <para>⚠ It is deliberately NOT a DispatcherQueue: nothing here marshals
/// WinUI objects, so a test using it must stay on ViewModel/machine code.</para>
/// </summary>
internal sealed class PumpedUiThread : SynchronizationContext, IDisposable
{
    private readonly BlockingCollection<(SendOrPostCallback Work, object? State)> _queue = new();
    private readonly Thread _thread;

    public PumpedUiThread()
    {
        _thread = new Thread(Pump) { IsBackground = true, Name = "fake-ui-thread" };
        _thread.Start();
    }

    private void Pump()
    {
        SetSynchronizationContext(this);
        foreach (var (work, state) in _queue.GetConsumingEnumerable())
        {
            try { work(state); }
            catch { /* a faulted work item must not kill the pump */ }
        }
    }

    public override void Post(SendOrPostCallback d, object? state) => _queue.Add((d, state));

    public override void Send(SendOrPostCallback d, object? state)
        => throw new NotSupportedException(
            "the real dispatcher's blocking Send would deadlock here too — post instead");

    /// <summary>Run <paramref name="work"/> on the pumped thread, wait for it, and
    /// rethrow whatever it threw.</summary>
    public void Run(Action work)
    {
        using var done = new ManualResetEventSlim();
        Exception? failure = null;
        Post(_ =>
        {
            try { work(); }
            catch (Exception ex) { failure = ex; }
            finally { done.Set(); }
        }, null);
        done.Wait();
        if (failure is not null) throw failure;
    }

    /// <summary>Occupy the thread until the returned handle is disposed. Returns only
    /// once the occupying work item is actually running, so a caller can rely on the
    /// thread being busy from here on.</summary>
    public IDisposable Occupy()
    {
        var release = new ManualResetEventSlim();
        var occupied = new ManualResetEventSlim();
        Post(_ => { occupied.Set(); release.Wait(); }, null);
        occupied.Wait();
        return new Releaser(release);
    }

    private sealed class Releaser(ManualResetEventSlim release) : IDisposable
    {
        public void Dispose() => release.Set();
    }

    public void Dispose()
    {
        _queue.CompleteAdding();
        _thread.Join(TimeSpan.FromSeconds(5));
        _queue.Dispose();
    }
}
