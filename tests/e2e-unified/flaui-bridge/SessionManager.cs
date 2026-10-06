using System.Diagnostics;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.UIA3;

namespace FauiBridge;

/// <summary>The app under test is gone, so no UIA question about it has an answer.
///
/// Deliberately its own type, and deliberately NOT swallowed by the catch-all in
/// <c>Actions.IsVisible</c>/<c>Count</c>: "the app crashed" answered as "the element
/// is not visible" (or "count = 0") is a lie of exactly the kind e2e convention 11
/// forbids — the test then fails on some unrelated assertion and the crash goes
/// unreported. Measured: FaunaApp exited 0xC0000409 (STATUS_STACK_BUFFER_OVERRUN)
/// and the whole session reported nothing but a bridge timeout 600s later.</summary>
class AppExitedException : Exception
{
    public AppExitedException(string message) : base(message) { }
}

class SessionManager : IDisposable
{
    private Process? _appProcess;
    private Application? _app;
    private UIA3Automation? _automation;

    /// <summary>What was launched and where it keeps its state — the two facts
    /// <see cref="ProcessScan.OwnersOfDataDir"/> needs to tell THIS session's app
    /// processes apart from a parallel session's (see <see cref="Quit"/>).</summary>
    private string? _appPath;
    private string? _dataDir;

    /// <summary>App processes that still owned this data dir after the last
    /// <see cref="Quit"/> killed its handle, each tagged <c>[UNTRACKED]</c> or
    /// <c>[the tracked handle]</c>. Non-empty means the run survived only because
    /// the sweep cleaned up after it; surfaced on <c>/session/status</c> so a
    /// harness leak is reported rather than silently absorbed.</summary>
    public IReadOnlyList<string> LastUntrackedSurvivors { get; private set; } = Array.Empty<string>();

    public AutomationElement RootElement => GetMainWindow(TimeSpan.FromSeconds(30));

    /// <summary>A main-window resolve slower than this narrates on stderr. EVERY
    /// element route re-resolves the window through this door before it searches,
    /// so a stalled resolve is charged to whichever find happened to run next and
    /// is otherwise indistinguishable from a slow search — the two halves of the
    /// same 120s peg need to be tellable apart from the log alone.</summary>
    private const double SlowWindowResolveSeconds = 5.0;

    /// <summary>The main window, with an injectable budget so the handle-lifetime
    /// self-test can exercise the same door a find goes through without paying the
    /// 30s wait a windowless stand-in app would otherwise cost.</summary>
    public AutomationElement GetMainWindow(TimeSpan timeout)
    {
        if (_app is null || _automation is null)
            throw new InvalidOperationException("No app attached");
        var sw = Stopwatch.StartNew();
        UiaStage.Enter($"SessionManager.GetMainWindow(budget={timeout.TotalSeconds:N0}s)");
        try
        {
            return GetMainWindowUntimed(timeout);
        }
        finally
        {
            sw.Stop();
            UiaStage.Enter("SessionManager.GetMainWindow (returned)");
            if (sw.Elapsed.TotalSeconds >= SlowWindowResolveSeconds)
                Console.Error.WriteLine(
                    $"[bridge] SLOW MAIN-WINDOW RESOLVE took {sw.Elapsed.TotalSeconds:N1}s " +
                    $"(budget {timeout.TotalSeconds:N0}s) — this is BEFORE any element " +
                    "search, so a find that looks slow may have spent its time here");
        }
    }

    /// <summary>Refuse to wait on a dead peer.
    ///
    /// This is the whole of row 182. FlaUI's own budget is a retry budget around
    /// calls that can each block, so a 30s argument does NOT bound the wait: a
    /// single GET /element/visible was measured holding the UIA gate for 623s
    /// against a process that had already exited, with 45 requests queued behind
    /// it — which is why the symptom appears in a later test's setup and teardown
    /// rather than where it happened, and why the app's crash was never reported
    /// at all. The bridge already had the answer (AppStatus) and simply never
    /// asked before waiting.</summary>
    private void RefuseIfAppExited(string what)
    {
        var (running, exitCode) = AppStatus();
        if (running || _appProcess is null) return;
        throw new AppExitedException(
            $"{what}: the app under test (pid {AppPid}) has exited" +
            (exitCode is null ? "" : $" with code {exitCode} (0x{exitCode:X8})") +
            " — no UIA query about it can be answered. This is the app's own " +
            "failure; the bridge is only reporting it promptly instead of waiting " +
            "out a budget the dead process will never satisfy.");
    }

    private AutomationElement GetMainWindowUntimed(TimeSpan timeout)
    {
        RefuseIfAppExited("resolving the main window");
        AutomationElement? window;
        try
        {
            window = _app!.GetMainWindow(_automation!, timeout);
        }
        catch (InvalidOperationException)
        {
            // The Application disposed the process handle IT owns (see
            // AttachWithRetry) and is now permanently unusable — every later call
            // throws while reading Process.Id. That used to end the session's finds
            // for good, silently costing feature coverage on pages that have nothing
            // to do with process lifecycle. Re-attach on a fresh handle and retry
            // once; the bridge's own handle is unaffected either way.
            Console.Error.WriteLine(
                "[bridge] FlaUI's Application lost the process handle it owns — re-attaching by pid.");
            RefuseIfAppExited("re-attaching after FlaUI lost the process handle");
            _app = AttachWithRetry(TimeSpan.FromSeconds(10));
            window = _app.GetMainWindow(_automation, timeout);
        }
        if (window is not null) return window;
        // No window AND the budget is spent: a crash DURING the resolve looks
        // identical to a slow start-up from here, so ask before blaming the app
        // for being slow. Checking after is what catches the mid-resolve case the
        // entry check cannot see.
        RefuseIfAppExited("waiting for the main window");
        throw new InvalidOperationException("Main window not found");
    }

    /// <summary>The pid this bridge launched, recorded at birth so it survives a
    /// handle that stops answering — see <see cref="AppHandleAnswers"/>.</summary>
    public int AppPid { get; private set; } = -1;

    /// <summary>Whether the tracked handle still answers for its process. The whole
    /// of row 41 is that this can go false while the app runs on.</summary>
    public bool AppHandleAnswers(out string why)
    {
        if (_appProcess is null) { why = "no tracked process"; return false; }
        try
        {
            _appProcess.Refresh();
            why = $"Id={_appProcess.Id} HasExited={_appProcess.HasExited}";
            return true;
        }
        catch (Exception ex)
        {
            why = $"{ex.GetType().Name}: {ex.Message}";
            return false;
        }
    }

    public UIA3Automation Automation => _automation
        ?? throw new InvalidOperationException("No automation instance");

    public void Launch(string appPath, string args, Dictionary<string, string>? environment = null,
                       string? packageFamilyName = null, string? packageAppId = null)
    {
        _automation = new UIA3Automation();
        _appPath = appPath;
        environment?.TryGetValue("FAUNA_E2E_DATA_DIR", out _dataDir);
        if (packageFamilyName is not null)
        {
            _appProcess = StartInPackageContext(
                appPath, args, environment ?? new(), packageFamilyName, packageAppId ?? "App");
        }
        else
        {
            var psi = new ProcessStartInfo(appPath)
            {
                Arguments = args,
                UseShellExecute = false,
            };
            if (environment != null) {
                foreach (var kv in environment) {
                    psi.EnvironmentVariables[kv.Key] = kv.Value;
                }
            }
            _appProcess = Process.Start(psi)
                ?? throw new InvalidOperationException($"Failed to start {appPath}");
        }
        AppPid = _appProcess.Id;
        // Which pid belongs to which session epoch, on the record. An untracked
        // survivor is identified by its epoch (Quit's scan reports one per process),
        // and that only names a culprit if the epochs this bridge itself started are
        // written down as they happen.
        string? epoch = null;
        environment?.TryGetValue("FAUNA_E2E_SESSION_EPOCH", out epoch);
        Console.Error.WriteLine(
            $"[bridge] launch epoch={epoch ?? "unset"} pid={AppPid} data_dir={_dataDir ?? "(none)"}");
        try
        {
            _app = AttachWithRetry(TimeSpan.FromSeconds(10));
        }
        catch
        {
            // Kill the process we started so it doesn't become orphaned
            try { _appProcess.Kill(); _appProcess.WaitForExit(5000); } catch { }
            _appProcess.Dispose();
            _appProcess = null;
            throw;
        }
    }

    /// <summary>How long the app may take to appear after a package-context launch
    /// returns — the wrapper's own PowerShell start plus the app's process creation.
    /// Named and generous (convention 14): a healthy launch resolves in a few
    /// hundred ms, so a green run pays nothing for the headroom.</summary>
    private static readonly TimeSpan PackagedStartBudget = TimeSpan.FromSeconds(60);

    /// <summary>Applies the session's environment, then starts the app. Runs INSIDE
    /// the package context (<c>Invoke-CommandInDesktopPackage</c>), so the app it
    /// starts inherits the package identity. Windows PowerShell 5.1, the one the
    /// cmdlet's <c>-Command</c> resolves on every Windows box.</summary>
    private const string PackagedLaunchPs1 = """
        param([string]$EnvFile, [string]$Exe, [string]$ArgFile)
        $vars = Get-Content -Raw -LiteralPath $EnvFile | ConvertFrom-Json
        foreach ($p in $vars.PSObject.Properties) {
            [Environment]::SetEnvironmentVariable($p.Name, [string]$p.Value, 'Process')
        }
        $a = Get-Content -Raw -LiteralPath $ArgFile
        if ($a -and $a.Trim()) { Start-Process -FilePath $Exe -ArgumentList $a.Trim() }
        else { Start-Process -FilePath $Exe }
        """;

    /// <summary>
    /// Start the app in a registered package's context, so it runs WITH that
    /// package's identity — the Store channel's launch shape (a full MSIX grants
    /// identity to its processes; <c>installers/windows.md</c> § Package identity
    /// for FaunaApp.exe).
    ///
    /// <para>Why not a plain <see cref="Process.Start(ProcessStartInfo)"/> of the exe
    /// inside the registered package: measured on <c>win</c> 2026-09-28, an exe started
    /// directly from a <c>-Register</c>ed loose layout has NO package identity
    /// (<c>GetPackageFullName</c> → <c>APPMODEL_ERROR_NO_PACKAGE</c>), while the same
    /// exe started through <c>Invoke-CommandInDesktopPackage</c> reports the package.
    /// That cmdlet does not carry the caller's environment into the child (measured
    /// the same day: a variable set in the calling shell is undefined in the
    /// packaged child), and it returns no pid — so it runs a small wrapper that reads
    /// the session's environment from a file and starts the app, and the app is
    /// then found by its exe path. Identity is inherited by the wrapper's child.</para>
    ///
    /// <para>The handoff files live in the bridge's own temp dir, which the packaged
    /// wrapper only READS: MSIX file virtualization redirects a packaged process's
    /// writes under <c>%LocalAppData%</c>, never its reads of files that exist.</para>
    /// </summary>
    private static Process StartInPackageContext(
        string appPath, string args, Dictionary<string, string> environment,
        string packageFamilyName, string packageAppId)
    {
        var fullPath = Path.GetFullPath(appPath);
        var handoff = Directory.CreateTempSubdirectory("fauna-bridge-pkg-").FullName;
        try
        {
            var script = Path.Combine(handoff, "launch.ps1");
            var envFile = Path.Combine(handoff, "env.json");
            var argFile = Path.Combine(handoff, "args.txt");
            File.WriteAllText(script, PackagedLaunchPs1);
            File.WriteAllText(envFile, System.Text.Json.JsonSerializer.Serialize(environment));
            File.WriteAllText(argFile, args);

            var started = DateTime.Now;
            var inner = $"-NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{script}\" "
                      + $"-EnvFile \"{envFile}\" -Exe \"{fullPath}\" -ArgFile \"{argFile}\"";
            var psi = new ProcessStartInfo("pwsh") { UseShellExecute = false, RedirectStandardError = true };
            foreach (var a in new[] {
                "-NoProfile", "-NonInteractive", "-Command",
                $"Invoke-CommandInDesktopPackage -PackageFamilyName '{packageFamilyName}' "
                + $"-AppId '{packageAppId}' -Command 'powershell.exe' -Args '{inner}'" })
            {
                psi.ArgumentList.Add(a);
            }
            using (var ps = Process.Start(psi)
                ?? throw new InvalidOperationException("Failed to start pwsh for a package-context launch"))
            {
                // Drained before the wait (convention 13): the one redirected pipe.
                var err = ps.StandardError.ReadToEnd();
                ps.WaitForExit();
                if (ps.ExitCode != 0)
                    throw new InvalidOperationException(
                        $"Invoke-CommandInDesktopPackage ({packageFamilyName}!{packageAppId}) "
                        + $"failed with exit code {ps.ExitCode}: {err}");
            }

            var deadline = DateTime.UtcNow + PackagedStartBudget;
            var name = Path.GetFileNameWithoutExtension(fullPath);
            while (true)
            {
                foreach (var p in Process.GetProcessesByName(name))
                {
                    try
                    {
                        if (string.Equals(p.MainModule?.FileName, fullPath, StringComparison.OrdinalIgnoreCase)
                            && p.StartTime >= started.AddSeconds(-1))
                        {
                            Console.Error.WriteLine(
                                $"[bridge] package-context launch {packageFamilyName}!{packageAppId} "
                                + $"pid={p.Id} package={PackageFullNameOf(p) ?? "(none)"}");
                            return p;
                        }
                    }
                    catch
                    {
                        // Another user's process, or one still coming up: not ours yet.
                    }
                    p.Dispose();
                }
                if (DateTime.UtcNow >= deadline)
                    throw new InvalidOperationException(
                        $"no {fullPath} process appeared within {PackagedStartBudget.TotalSeconds:N0}s "
                        + $"of the package-context launch ({packageFamilyName}!{packageAppId})");
                Thread.Sleep(200);
            }
        }
        finally
        {
            try { Directory.Delete(handoff, recursive: true); } catch { }
        }
    }

    [System.Runtime.InteropServices.DllImport("kernel32.dll", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
    private static extern int GetPackageFullName(IntPtr hProcess, ref uint length, System.Text.StringBuilder? name);

    /// <summary>The package identity <paramref name="p"/> runs with, or null when it
    /// has none — the OS's own answer, read off the process token.</summary>
    private static string? PackageFullNameOf(Process p)
    {
        try
        {
            uint len = 0;
            // 15700 = APPMODEL_ERROR_NO_PACKAGE; 122 = ERROR_INSUFFICIENT_BUFFER.
            if (GetPackageFullName(p.Handle, ref len, null) != 122) return null;
            var sb = new System.Text.StringBuilder((int)len);
            return GetPackageFullName(p.Handle, ref len, sb) == 0 ? sb.ToString() : null;
        }
        catch
        {
            return null;
        }
    }

    /// <summary>The launched app's package identity (null: none, or no app) —
    /// surfaced on <c>/session/status</c> so a test asserting a packaged launch
    /// can check the premise instead of trusting it.</summary>
    public string? AppPackageFullName() => _appProcess is null ? null : PackageFullNameOf(_appProcess);

    /// <summary>
    /// Attach FlaUI to the launched process, through a handle opened for FlaUI
    /// alone, retrying until its main module is enumerable. <c>Application.Attach</c> reads
    /// <c>process.MainModule.FileName</c> (via FlaUI's <c>GetMainModuleFilepath</c>),
    /// which is null for the first few hundred ms after <see cref="Process.Start"/>
    /// — especially on win-arm64 — so a bare Attach intermittently throws
    /// <see cref="NullReferenceException"/>. Retry on any attach fault until the
    /// deadline; surface a clear error if the process exits during launch.
    ///
    /// <para><b>FlaUI gets a handle OF ITS OWN, never the bridge's — row 41.</b>
    /// Passing <c>_appProcess</c> straight to <c>Application.Attach</c> shared ONE
    /// object between two owners, and FlaUI disposes it:
    /// <c>Application.GetMainWindow</c> → <c>WaitWhileMainHandleIsMissing</c> calls
    /// <c>Dispose()</c> on the process it holds whenever the main window handle is
    /// missing (proven by stack trace; reproduced by <see cref="SelfTest"/>). The
    /// bridge's handle then threw <see cref="InvalidOperationException"/> for the rest
    /// of the session, so <see cref="Quit"/>'s kill loop read a live app as "already
    /// gone" and left it running against the session's data dir. A separate
    /// <see cref="Process"/> object for the same pid keeps FlaUI on the exact same
    /// code path — same store-app detection, same main-module read — while giving it
    /// something of its own to dispose.</para>
    /// </summary>
    private Application AttachWithRetry(TimeSpan timeout)
    {
        var deadline = DateTime.UtcNow + timeout;
        while (true)
        {
            if (_appProcess is null)
                throw new InvalidOperationException("No app process to attach to.");
            _appProcess.Refresh();
            if (_appProcess.HasExited)
                // Reached from Launch AND from a mid-session re-attach, so it must
                // not claim a phase: "during launch" would misdirect a reader whose
                // app died at test 8.
                throw new InvalidOperationException(
                    $"App process (pid {AppPid}) has exited (exit code {_appProcess.ExitCode}).");
            var theirs = Process.GetProcessById(AppPid);
            try
            {
                return Application.Attach(theirs);
            }
            catch when (DateTime.UtcNow < deadline)
            {
                // Theirs to own only once Attach accepts it; a rejected one is ours
                // to close, or every retry leaks a process handle.
                theirs.Dispose();
                Thread.Sleep(200);
            }
        }
    }

    /// <summary>
    /// Whether the launched app is still alive, and its exit code if not — the only
    /// way the python side can observe a process THIS bridge owns.
    ///
    /// <para>Needed by the (OS login, account) single-instance guard's e2e leg
    /// (<c>account-scoping.md</c> § Concurrent instances), whose whole observable is
    /// "a refused launch is process DEATH, never a survivor serving the wrong
    /// account". Without it the shared <c>expect_launch_refused</c> helper finds no
    /// process handle on the windows driver and returns <b>vacuously green</b> — it
    /// would pass against an app that launched perfectly.</para>
    ///
    /// <para>Deliberately NOT implemented by redirecting the child's stderr: an
    /// inherited-but-undrained pipe is the exact wedge that cost seven sessions in
    /// 2026-07 (e2e convention 13 in <c>testing.md</c>). The refusal's causal
    /// evidence is read from the app's own on-disk log instead.</para>
    /// </summary>
    /// <summary>
    /// Every top-level window the launched app currently owns, with each one's
    /// offscreen flag — the OS's own answer to "did a window open?", independent
    /// of anything the app says about itself.
    ///
    /// <para>Exists for the <c>--autostart</c> tray-residency case
    /// (<c>apps/windows.md</c> § App Lifecycle → Auto-start at sign-in), whose
    /// contract is that a sign-in launch landing <c>Online</c> opens NO window.
    /// The app publishes its own activation decision into the e2e state protocol,
    /// but a self-report can only prove the code took the branch it meant to —
    /// this proves the branch had the effect it claims. Reporting <i>every</i>
    /// window with its title and offscreen flag (rather than a bare count) is what
    /// lets the assertion name what actually opened instead of failing as
    /// "expected 0, got 1".</para>
    ///
    /// <para>Never throws: an app with no window at all is the EXPECTED state in
    /// one arm of that case, and FlaUI's enumeration faults while a process is
    /// still coming up. Both are reported as "no windows", which is what they
    /// mean — the caller distinguishes them with the process-liveness route.</para>
    /// </summary>
    public IReadOnlyList<(string Title, bool IsOffscreen)> TopLevelWindows()
    {
        if (_app is null || _automation is null)
        {
            return Array.Empty<(string, bool)>();
        }
        try
        {
            return Enumerate(_app);
        }
        catch (InvalidOperationException)
        {
            // Same disassociation as GetMainWindow (see AttachWithRetry). Re-attach
            // rather than report "no windows": this method's caller asserts that NO
            // window opened, so a lost handle would turn a real regression into a
            // vacuous pass.
            try
            {
                _app = AttachWithRetry(TimeSpan.FromSeconds(10));
                return Enumerate(_app);
            }
            catch
            {
                return Array.Empty<(string, bool)>();
            }
        }
        catch
        {
            return Array.Empty<(string, bool)>();
        }

        List<(string, bool)> Enumerate(Application app) => app
            .GetAllTopLevelWindows(_automation)
            .Select(w => (w.Title ?? string.Empty, w.IsOffscreen))
            .ToList();
    }

    /// <summary>Every live app process owning this session's data dir — the OS's
    /// answer to "how many instances are there really?", which no process handle can
    /// give. See <see cref="Quit"/> for why the data dir, not the handle, is the
    /// identity that matters.</summary>
    public IReadOnlyList<ProcessScan.AppInstance> DataDirOwners() =>
        ProcessScan.OwnersOfDataDir(_appPath, _dataDir);

    public (bool Running, int? ExitCode) AppStatus()
    {
        if (_appProcess is null)
        {
            return (false, null);
        }
        try
        {
            _appProcess.Refresh();
            return _appProcess.HasExited
                ? (false, _appProcess.ExitCode)
                : (true, null);
        }
        catch
        {
            // A process we can no longer interrogate is not a running one.
            return (false, null);
        }
    }

    /// <summary>How long a killed app gets to actually leave the process table.</summary>
    private const int ExitBudgetMs = 10_000;

    /// <summary>
    /// Kill the launched app and <b>verify</b> it is gone. Returns false if a
    /// process survived the whole budget.
    ///
    /// <para>The verification is load-bearing, not defensive tidiness. windows'
    /// <c>recover()</c> relaunches against the SAME <c>FAUNA_E2E_DATA_DIR</c> and
    /// credential store (unlike linux's fresh-mkdtemp relaunch), so a survivor
    /// still holds the per-account instance lock — the next process then takes the
    /// <c>[launch-collision] … already served by a live instance</c> branch, shows
    /// the account chooser, and never reaches <c>TestAgent.Configure()</c>. Its
    /// agent never polls and never pushes state, while the SURVIVOR's pushes are
    /// discarded by the bridge's epoch fence, so <c>/app/state</c> stays empty and
    /// every <c>reset()</c> dies as "App did not acknowledge reset within 10.0s".
    /// The fixture's retry then relaunches on top again, stacking a third live
    /// instance, so one un-verified kill loses every remaining test module in the
    /// session (21/40 tests in one batch, six whole files).</para>
    ///
    /// <para>The old code killed inside <c>try {} catch {}</c>, waited inside
    /// another, and reported success unconditionally — so the survivor was
    /// invisible and the wedge presented as an unrelated per-test timeout.
    /// <c>entireProcessTree</c> because a bare <c>Kill()</c> leaves any child the
    /// app spawned (e.g. <c>InstanceSpawner</c>) holding the same stores.</para>
    ///
    /// <para><b>Verifying the handle is not enough — that was the fourth wrong
    /// diagnosis.</b> With the kill verified, the wedge still reproduced: the lock
    /// was held by a process the bridge's handle never covered (state pushes kept
    /// arriving stamped with an epoch 2–3 relaunches old, while the tracked handle
    /// had genuinely exited). <c>Kill(entireProcessTree: true)</c> walks children by
    /// their recorded parent, so an instance whose parent already died is invisible
    /// to it, and <c>HasExited</c> on one handle can only ever speak for one process.
    /// So the question asked here is the one that actually matters for the next
    /// launch: <b>does any live app process own this data dir?</b>
    /// (<see cref="ProcessScan"/> — matched on <c>FAUNA_E2E_DATA_DIR</c>, which is a
    /// per-driver <c>mkdtemp</c>, so a parallel session's app can never match and can
    /// never be touched.)</para>
    /// </summary>
    public bool Quit(bool sweepDataDir = true)
    {
        var exited = true;
        var trackedPid = -1;
        if (_appProcess is null)
        {
            // Not a tidy no-op: this is the state the whole sweep exists to survive.
            // Runs 2/3 of row 39 showed EVERY relaunch reaching Quit here — kill loop
            // never entered, "closed" reported — while the app this session launched
            // was still alive and holding the account lock. Saying so out loud is what
            // separates "there was nothing to kill" from "we lost the handle to
            // something that is still running".
            Console.Error.WriteLine(
                "[bridge] Quit with NO tracked app process — the kill loop is skipped " +
                "entirely; only the data-dir sweep below can still find the app.");
        }
        if (_appProcess is not null)
        {
            for (var attempt = 0; attempt < 2; attempt++)
            {
                try
                {
                    _appProcess.Refresh();
                    if (_appProcess.HasExited) break;
                    _appProcess.Kill(entireProcessTree: true);
                }
                catch (InvalidOperationException ex)
                {
                    // "Already gone" is the usual reading — but a disassociated or
                    // disposed handle throws exactly the same way while its process
                    // runs on, so the two must not be conflated silently. The sweep
                    // decides which it was; this records that the handle stopped
                    // answering, and why.
                    Console.Error.WriteLine(
                        $"[bridge] app handle stopped answering ({ex.GetType().Name}: {ex.Message}) " +
                        "— treating the kill loop as finished; the data-dir sweep is now the " +
                        "only thing that can tell an exited app from a live one.");
                    break;
                }
                catch (Exception ex)
                {
                    Console.Error.WriteLine(
                        $"[bridge] Kill of app pid {AppPid} threw: {ex.Message}");
                }
                try { _appProcess.WaitForExit(ExitBudgetMs); } catch { }
                try { _appProcess.Refresh(); } catch { }
            }
            try { exited = _appProcess.HasExited; } catch { exited = true; }
            if (!exited)
            {
                Console.Error.WriteLine(
                    $"[bridge] app pid {AppPid} SURVIVED kill — refusing to " +
                    "report the session closed (a relaunch on top of it would wedge the run).");
            }
            trackedPid = AppPid;
        }
        try { _appProcess?.Dispose(); } catch { }
        try { _automation?.Dispose(); } catch { }
        _appProcess = null;
        _app = null;
        _automation = null;

        // Deliberately NOT `exited && ClearDataDirOwners()`: `&&` would short-circuit
        // in precisely the case the sweep exists for. When there IS a data dir, the
        // sweep's answer supersedes the handle's — it sees every owner, including
        // ones no handle covers, and has just killed them, so a tracked process that
        // shrugged off `Kill` is cleared by it rather than ending the run. The handle
        // remains the verdict only for a launch that never isolated a data dir, where
        // there is nothing to scan.
        // A caller that shares its data dir with a sibling instance ANOTHER bridge
        // tracks (a two-instance collision test stopping only the first) opts out:
        // the sibling is not a leak, and its own bridge sweeps it at its teardown.
        if (!sweepDataDir) return exited;
        var dirCleared = ClearDataDirOwners(trackedPid);
        return _dataDir is null ? exited : dirCleared;
    }

    /// <summary>
    /// Kill every live app process still owning this session's data dir, and report
    /// whether the dir ended up unowned. See <see cref="Quit"/> for why this, and not
    /// the tracked handle, is the condition the next launch actually depends on.
    ///
    /// <para>Loud on purpose. A survivor here is a harness leak — some path started
    /// an app instance the bridge never tracked — and the whole reason row 39 took
    /// four sessions is that the wedge was silent. Killing it un-wedges the run;
    /// printing its pid, its parent and the epoch it was launched under is what
    /// lets the leak itself be found instead of papered over. The parent pid is the
    /// creator recorded at birth, so it names the spawner even after the spawner has
    /// died. <paramref name="trackedPid"/> is tagged rather than excluded, because
    /// "the handle we held shrugged off Kill" and "an instance we never knew about"
    /// are different bugs and the log has to say which one happened.</para>
    /// </summary>
    private bool ClearDataDirOwners(int trackedPid)
    {
        LastUntrackedSurvivors = Array.Empty<string>();
        if (_dataDir is null || _appPath is null) return true;

        var owners = ProcessScan.OwnersOfDataDir(_appPath, _dataDir);
        if (owners.Count == 0) return true;

        LastUntrackedSurvivors = owners
            .Select(o => ProcessScan.Describe(new[] { o })
                         + (o.Pid == trackedPid ? " [the tracked handle]" : " [UNTRACKED]"))
            .ToList();
        Console.Error.WriteLine(
            $"[bridge] app instance(s) still own data dir {_dataDir} after killing tracked " +
            $"pid {(trackedPid < 0 ? "(none)" : trackedPid.ToString())}: " +
            $"{string.Join("; ", LastUntrackedSurvivors)}. Killing them so the next launch " +
            "does not land in the account chooser.");

        foreach (var owner in owners)
        {
            try
            {
                using var p = Process.GetProcessById(owner.Pid);
                p.Kill(entireProcessTree: true);
                p.WaitForExit(ExitBudgetMs);
            }
            catch (ArgumentException) { /* already gone between scan and kill */ }
            catch (InvalidOperationException) { /* ditto */ }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"[bridge] kill of untracked pid {owner.Pid} threw: {ex.Message}");
            }
        }

        var remaining = ProcessScan.OwnersOfDataDir(_appPath, _dataDir);
        if (remaining.Count == 0) return true;
        Console.Error.WriteLine(
            $"[bridge] data dir {_dataDir} is STILL owned by {ProcessScan.Describe(remaining)} — " +
            "refusing to report the session closed.");
        return false;
    }

    public void Dispose()
    {
        Quit();
    }
}
