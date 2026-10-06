using System;
using System.IO;
using System.Threading;
using FaunaApp.Core.Logs;

namespace FaunaApp.Core.Services;

/// <summary>
/// The payload twin of the single-instance activate event: how a second launch
/// carrying a <c>fauna://</c> route (the Explorer <b>Share</b> leaf's hand-off,
/// <c>docs/goal/architecture/apps/windows.md</c> § Shell Extension → <i>The Share
/// hand-off</i>, step 3) gives that route to the instance already running, then exits.
///
/// <para><b>Why a file beside the event.</b> An auto-reset <see cref="EventWaitHandle"/>
/// carries no payload — the reason the add-account intent got its own named event. A
/// route does carry one (which file, which folder), so the forwarder writes it to a
/// per-user pending-route file first and only then signals; the listener takes (reads
/// and deletes) the file on the wake-up. Last writer wins: two routes forwarded before
/// the primary drains are one user clicking twice, and the newer click is the one they
/// meant.</para>
///
/// <para><b>What the payload may do.</b> Nothing but navigate — the caller applies a
/// route by opening a surface the user must still act on (the goal doc's step 4). So a
/// file another process of this user wrote can at worst open a page; the file lives
/// under <c>%LocalAppData%</c>, which only this user can write.</para>
///
/// <para>Headless <c>System.Threading</c> + file IO, so tests drive both ends directly;
/// "raise the window and navigate" arrives as a callback, the
/// <see cref="AccountActivationEndpoint"/> shape. Best-effort throughout: a guard
/// fault never breaks a launch.</para>
/// </summary>
public sealed class RouteHandoffEndpoint : IDisposable
{
    private const string LogSource = "RouteHandoffEndpoint";

    /// <summary>The app-wide event name. <c>Local\</c> scopes it to the OS logon
    /// session, like the activate event it sits beside.</summary>
    public const string DefaultEventName = @"Local\FaunaApp-OpenRoute";

    /// <summary>The pending-route file's name under the per-user Fauna data dir.</summary>
    public const string PendingFileName = "pending-route";

    /// <summary>The app's pending-route file: <c>%LocalAppData%\Fauna\pending-route</c>.</summary>
    public static string DefaultPendingPath => Path.Combine(BackupPaths.DataDir, PendingFileName);

    private readonly string _eventName;
    private readonly string _pendingPath;
    private readonly Action<string> _onRoute;
    private EventWaitHandle? _handle;

    /// <param name="eventName">The event to own — <see cref="DefaultEventName"/> in the app,
    /// a unique name under test.</param>
    /// <param name="pendingPath">The pending-route file.</param>
    /// <param name="onRoute">Invoked on the listener thread with each route taken. The
    /// caller marshals to the UI thread.</param>
    public RouteHandoffEndpoint(string eventName, string pendingPath, Action<string> onRoute)
    {
        _eventName = eventName;
        _pendingPath = pendingPath;
        _onRoute = onRoute ?? throw new ArgumentNullException(nameof(onRoute));
    }

    /// <summary>Own the event and start listening. <c>false</c> when it could not be
    /// claimed — a forwarder then reports "nobody listening" and falls back to a plain
    /// raise.</summary>
    public bool Start()
    {
        try
        {
            var ev = new EventWaitHandle(false, EventResetMode.AutoReset, _eventName, out _);
            _handle = ev;
            var thread = new Thread(() => Listen(ev)) { IsBackground = true, Name = "FaunaRouteListener" };
            thread.Start();
            return true;
        }
        catch (Exception ex)
        {
            ShellLog.Warn(LogSource, $"[route] could not claim {_eventName} ({ex.Message})");
            return false;
        }
    }

    /// <summary>
    /// Hand <paramref name="uri"/> to the running instance that owns
    /// <paramref name="eventName"/>. <c>true</c> if it was delivered.
    ///
    /// <para>The owner is probed FIRST (<c>TryOpenExisting</c>, never a create — creating
    /// would succeed against an instance that does not exist): with nobody listening,
    /// nothing is written, so no stale route waits to surprise a later launch.</para>
    /// </summary>
    public static bool TryForward(string eventName, string pendingPath, string uri)
    {
        try
        {
            if (!EventWaitHandle.TryOpenExisting(eventName, out var ev))
            {
                return false;
            }
            using (ev)
            {
                Directory.CreateDirectory(Path.GetDirectoryName(pendingPath)!);
                // Write-then-rename, so the listener never reads a half-written route.
                var tmp = pendingPath + ".tmp";
                File.WriteAllText(tmp, uri);
                File.Move(tmp, pendingPath, overwrite: true);
                ev.Set();
            }
            return true;
        }
        catch (Exception ex)
        {
            ShellLog.Warn(LogSource, $"[route] forward failed ({ex.Message})");
            return false;
        }
    }

    /// <summary>Read and delete the pending route; <c>null</c> when there is none.</summary>
    public static string? Take(string pendingPath)
    {
        try
        {
            if (!File.Exists(pendingPath)) return null;
            var uri = File.ReadAllText(pendingPath).Trim();
            File.Delete(pendingPath);
            return uri.Length == 0 ? null : uri;
        }
        catch (Exception ex)
        {
            ShellLog.Warn(LogSource, $"[route] take failed ({ex.Message})");
            return null;
        }
    }

    /// <summary>Stop listening. Wakes the listener rather than disposing the handle
    /// under its wait (<see cref="AccountActivationEndpoint"/>'s <c>Release</c> note:
    /// a disposed-while-waiting handle keeps the name owned).</summary>
    public void Dispose()
    {
        var ev = _handle;
        _handle = null;
        try { ev?.Set(); } catch { /* already torn down */ }
    }

    private void Listen(EventWaitHandle handle)
    {
        try
        {
            while (true)
            {
                handle.WaitOne();
                if (!ReferenceEquals(handle, _handle)) return; // Dispose's wake-up
                // A wake-up with no file is the second of two back-to-back forwards
                // whose routes the first take already drained (last writer wins).
                if (Take(_pendingPath) is { } uri)
                {
                    ShellLog.Info(LogSource, "[route] received a forwarded route");
                    _onRoute(uri);
                }
            }
        }
        catch
        {
            // Process tearing down — stop listening.
        }
        finally
        {
            handle.Dispose();
        }
    }
}
