using System.Net;

namespace FauiBridge;

/// <summary>
/// Picks a free loopback port for the bridge's own HTTP listener, retrying on a bind
/// conflict instead of the one-shot ``new Random().Next(18000, 19000)`` + single
/// ``Start()`` this replaces (`e2e-conventions.md` § convention 10, "any harness child
/// that picks its own listening port…" — added in the same commit as this fix).
///
/// <para><b>Two bridges can draw the same port.</b> The two-seat fixture launches an
/// initiator seat and a recipient seat back to back, each its own bridge process, and
/// any other bridge alive on the box (a sibling session's Windows run) counts too.
/// Measured 2026-09-21/22: 3 collisions in 240 concurrent launches (30 at once × 8
/// trials) — the odds the single-shot draw always had, ≈ (bridges alive)/1000.</para>
///
/// <para><b>Why a bind-conflict FAMILY, not one code.</b> The measured production
/// collision — a second <see cref="HttpListener"/>/http.sys registration already
/// owning the port — throws <see cref="HttpListenerException"/> with native code 183
/// (ERROR_ALREADY_EXISTS). A PLAIN socket owning the port instead — the shape
/// <c>web-bridge/server.py</c> would collide in, since it draws from the identical
/// 18000-19000 range on the same box — throws a DIFFERENT code, 32
/// (ERROR_SHARING_VIOLATION), confirmed experimentally 2026-09-22 (a scratch
/// <c>HttpListener.Start()</c> against a port held by a raw <see cref="System.Net.Sockets.Socket"/>
/// vs. one held by a second <see cref="HttpListener"/>). Hard-coding 183 alone would
/// silently stop retrying on the 32 case.</para>
///
/// <para><b>Why draw+bind are injected, not called directly.</b> That is what lets
/// <see cref="SelfTest.PortRetryChecks"/> pin the retry policy with no real listener,
/// no port, and no process: a scripted draw sequence and a scripted (conflict, port)
/// tryStart never touch a socket.</para>
/// </summary>
internal static class PortPicker
{
    internal const int RangeStart = 18000;
    internal const int RangeEnd = 19000; // exclusive — matches the draw this replaces.

    /// <summary>Bounded, not unbounded — a stuck box should fail loudly, not hang.</summary>
    internal const int MaxAttempts = 20;

    /// <summary>Native Win32 codes <see cref="HttpListener.Start"/> raises on a bind
    /// conflict. 183 = ERROR_ALREADY_EXISTS (another http.sys registration — another
    /// HttpListener). 32 = ERROR_SHARING_VIOLATION (a non-http.sys holder — a plain
    /// socket, e.g. the web bridge's <c>HTTPServer</c>).</summary>
    internal static bool IsBindConflict(int nativeErrorCode) =>
        nativeErrorCode == 183 || nativeErrorCode == 32;

    /// <summary>
    /// Draws a port via <paramref name="draw"/> and attempts to bind it via
    /// <paramref name="tryStart"/>, retrying up to <see cref="MaxAttempts"/> times —
    /// on a FRESH port each time, never the one that just conflicted — whenever
    /// <paramref name="tryStart"/> throws an <see cref="HttpListenerException"/> whose
    /// <see cref="HttpListenerException.NativeErrorCode"/> is in the
    /// <see cref="IsBindConflict"/> family. Any other exception propagates immediately
    /// — a bind conflict is the only failure this retries.
    /// </summary>
    /// <exception cref="InvalidOperationException">every attempt conflicted.</exception>
    internal static (int port, T result) BindFreshPort<T>(Func<int> draw, Func<int, T> tryStart)
    {
        HttpListenerException? last = null;
        for (var attempt = 0; attempt < MaxAttempts; attempt++)
        {
            var port = draw();
            try
            {
                return (port, tryStart(port));
            }
            catch (HttpListenerException ex) when (IsBindConflict(ex.NativeErrorCode))
            {
                last = ex;
                // Fall through and draw again — never retry the same port.
            }
        }
        throw new InvalidOperationException(
            $"could not bind a free port in {MaxAttempts} attempts (range {RangeStart}-{RangeEnd})",
            last);
    }
}
