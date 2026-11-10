using System;
using System.Collections.Generic;
using System.Threading.Tasks;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// A per-key cache of loaded values that also shares a load <b>still running</b>: every
/// caller that asks for a key while its load is in flight awaits that one load instead of
/// starting its own.
///
/// <para><b>Why the in-flight half matters.</b> A cache that remembers a value only once its
/// load has finished starts a new load for every caller that asks in the meantime. The feed
/// asks for each picture many times while it loads — every snapshot tick of a hydrating feed
/// re-runs the post-card sync, which requests each image row's C2PA verdict until it lands,
/// and a content-changed row re-binds its image — so on a feed with a dozen pictures the old
/// shape ran hundreds of redundant fetch, open, C2PA-parse and decode passes a second, enough
/// UI-thread work to stall the app (<c>SingleFlightCacheTests</c> carries the measurement).</para>
///
/// <para><b><see cref="Clear"/> is a boundary, not just an eviction.</b> The picture cache is
/// cleared when the feed manager changes, because a tier-restricted post's picture is opened
/// with a per-post key only that actor's manager holds. So a clear also forgets the loads in
/// flight — a caller after it never awaits a load begun before it — and a load that finishes
/// after a clear never writes into the cleared cache.</para>
///
/// <para>A value <paramref name="keep"/> rejects (a picture that did not open yet, a failed
/// decode) is handed to the callers that shared its load and then forgotten, so the next
/// caller loads again; a load that throws is forgotten the same way. Loads start on the
/// caller's context, exactly as a direct call would.</para>
/// </summary>
public sealed class SingleFlightCache<TValue>
{
    private readonly object _gate = new();
    private readonly Dictionary<string, TValue> _done = new();
    private readonly Dictionary<string, Task<TValue>> _inFlight = new();
    private readonly Func<TValue, bool> _keep;

    /// <summary>Bumped by <see cref="Clear"/>; a load records the generation it began
    /// under and stores its value only if no clear has happened since.</summary>
    private int _generation;

    /// <param name="keep">Whether a finished value is worth keeping for later callers.</param>
    public SingleFlightCache(Func<TValue, bool> keep) => _keep = keep;

    /// <summary>The cached value for <paramref name="key"/>, else the load already running
    /// for it, else a new load started with <paramref name="load"/>.</summary>
    public Task<TValue> GetOrLoadAsync(string key, Func<Task<TValue>> load)
    {
        TaskCompletionSource<TValue> started;
        int generation;
        lock (_gate)
        {
            if (_done.TryGetValue(key, out var hit)) return Task.FromResult(hit);
            if (_inFlight.TryGetValue(key, out var running)) return running;
            // Registered BEFORE the load starts, so a load that completes synchronously
            // still finds — and removes — its own entry.
            started = new TaskCompletionSource<TValue>(TaskCreationOptions.RunContinuationsAsynchronously);
            _inFlight[key] = started.Task;
            generation = _generation;
        }
        _ = RunAsync(key, load, started, generation);
        return started.Task;
    }

    /// <summary>Forget every cached value and every load in flight (see the class note).</summary>
    public void Clear()
    {
        lock (_gate)
        {
            _done.Clear();
            _inFlight.Clear();
            _generation++;
        }
    }

    private async Task RunAsync(
        string key, Func<Task<TValue>> load, TaskCompletionSource<TValue> started, int generation)
    {
        TValue value;
        try
        {
            value = await load();
        }
        catch (Exception ex)
        {
            lock (_gate) Forget(key, started.Task);
            started.SetException(ex);
            return;
        }
        lock (_gate)
        {
            Forget(key, started.Task);
            if (generation == _generation && _keep(value))
                _done[key] = value;
        }
        started.SetResult(value);
    }

    /// <summary>Drop <paramref name="key"/>'s in-flight entry only if it is still this
    /// load's — after a clear a newer load may own the key.</summary>
    private void Forget(string key, Task<TValue> mine)
    {
        if (_inFlight.TryGetValue(key, out var current) && ReferenceEquals(current, mine))
            _inFlight.Remove(key);
    }
}
