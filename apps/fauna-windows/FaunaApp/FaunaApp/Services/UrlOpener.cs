using System;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using Windows.System;

namespace FaunaApp.Services;

/// <summary>
/// Hand a URL to the OS default handler — the ONE opener resolution every
/// open-something-external call site in the windows shell shares (the provider
/// links on both credential forms, the `hosted-auth` verification URL, the feed's
/// unlock-offer payment URL, the profile's subscription payment URL).
///
/// <para>The C# twin of tui's <c>os_open</c> module, and for its stated reason:
/// "the OS already owns 'which program opens a URL'", so fauna adds no
/// program-picker knob — <b>the only injection point is the e2e suppression
/// below</b>. Five call sites had each spelled
/// <c>Uri.TryCreate(...) &amp;&amp; Launcher.LaunchUriAsync(uri)</c> inline, which is
/// how the gate decays: every new seam is a fresh chance to forget it.</para>
///
/// <para><b>Fire-and-forget, deliberately never awaited</b> — the shape tui
/// (<c>open_in_browser</c>: "failure is silent by design") and linux
/// (<c>let _ = AppInfo::launch_default_for_uri(...)</c>) already use. Awaiting lets a
/// missing or hung default-handler association — a real risk on a dev box with no
/// browser set as default, where <c>LaunchUriAsync</c> can block on an "Open with…"
/// picker no automated run can dismiss — stall whatever the caller does next. No
/// caller has ever read the returned success flag.</para>
///
/// <para><b>⚠ Under a harness launch this does NOT reach the OS, and that is
/// load-bearing</b> (e2e-conventions.md point 10: "an app launch is isolated from
/// the box it runs on" — drivers close every inherited channel by default). The OS
/// browser is an inherited channel of the worst kind: it steals foreground from the
/// app under test, it outlives the run, and — measured — <b>it wedges the test's own
/// fake server</b>. `test_bundled_provider.py --app windows` opened the bundled
/// provider's verification URL in the real default browser; the browser fetched
/// `/activate` and `/favicon.ico` from the `fake_cloud` server and then held its
/// keep-alive/preconnect sockets, and that server stops answering **entirely** once
/// about six client connections sit idle on it (measured black-box: 0.02 s served
/// with one squatter, a 30 s timeout with six). The app's very next request — the
/// device-flow token poll — was then not served for 86 s, with the app's own log
/// showing the sleep waking on time and the request issued: the stall was in front
/// of the fake, not in the app. Suppressing
/// the launch is what makes that impossible rather than unlikely.
///
/// The handoff stays OBSERVABLE rather than silent: the URL is logged at this seam,
/// so a test can still assert that the app reached the OS with the right address —
/// which is more than the five inline call sites ever offered, and covers the
/// failure this replaces in the other direction (tui's own note records a windows
/// arm that was simply missing, so its provider links silently never opened).
/// Redaction (observability.md): a provider signup / verification / payment URL is
/// an address the user is about to see in their own browser, never a secret.</para>
/// </summary>
internal static class UrlOpener
{
    /// <summary>
    /// Hand <paramref name="url"/> to the OS default handler, or — under a harness
    /// launch — record it instead of launching. A URL that will not parse as
    /// absolute is dropped, exactly as each inline <c>Uri.TryCreate</c> guard
    /// already dropped it.
    /// </summary>
    /// <param name="source">The <c>ShellLog</c> source tag of the calling surface,
    /// so a trace says WHICH handoff fired.</param>
    internal static void Open(string? url, string source)
    {
        if (!Uri.TryCreate(url, UriKind.Absolute, out var uri))
        {
            ShellLog.Warn(source, $"open-url: refused a non-absolute address ({url?.Length ?? 0} chars)");
            return;
        }

        // Convention 15: the env read is E2eEnv's alone (pinned by
        // test_ffi_flavor_split.py::test_no_windows_app_source_reads_a_fauna_e2e_var_directly),
        // and the whole branch compiles out of a release artifact with it.
        if (E2eEnv.Bridge is not null)
        {
            ShellLog.Info(source, $"open-url: suppressed under the e2e harness: {uri}");
            return;
        }

        ShellLog.Info(source, $"open-url: handing {uri.Scheme}://{uri.Host} to the OS");
        try { _ = Launcher.LaunchUriAsync(uri); }
        catch (Exception e)
        {
            // No handler association, or the shell refused the launch. Silent to the
            // user by design (every caller paints the address on screen as well), but
            // never silent to the log — a swallow that says nothing is the class
            // ShellLog exists to end.
            ShellLog.Warn(source, $"open-url: the OS refused the launch ({e.GetType().Name})");
        }
    }
}
