using System;
using System.Runtime.InteropServices;
using FaunaApp.Core.Services;

namespace FaunaApp.Services;

/// <summary>
/// Windows Restart-Manager cooperative shutdown (apps/windows.md § App Lifecycle,
/// "Cooperative shutdown for installers"). Two pieces:
///
/// <list type="number">
/// <item>Registers the process for OS restart (<c>RegisterApplicationRestart</c>),
///   so after a Restart-Manager-driven close (an MSI install/upgrade that needs the
///   app's binaries) Windows relaunches it automatically.</item>
/// <item>Creates a <b>persistent hidden top-level window</b> that receives the OS
///   session-end messages (<c>WM_QUERYENDSESSION</c> / <c>WM_ENDSESSION</c>) and, on
///   them, persists unsaved compose drafts (principle 2) and quits gracefully — so
///   the installer/RM never has to force-kill the app, and the installer's
///   <c>taskkill</c> CA (installers/windows.md) drops to a pure last-resort net.</item>
/// </list>
///
/// <para><b>Why a dedicated top-level window</b> and not the
/// <see cref="TrayIconService"/> callback window: that one is a <i>message-only</i>
/// window (<c>HWND_MESSAGE</c> parent), which by Win32 design never receives
/// session-end messages — those go to <i>top-level</i> windows. A persistent,
/// never-shown top-level window also guarantees the process always has a window to
/// receive the RM shutdown <b>even when the main window is hidden to the tray</b>
/// (close-to-tray on) — exactly the windowless-instance state the installer's
/// force-kill was patching over.</para>
///
/// <para>The policy (whether to register, which messages are shutdown requests)
/// is the pure, unit-tested <see cref="RestartManagerGate"/>; this class is only the
/// platform mechanics. <b>Disabled under the E2E bridge</b> (<c>FAUNA_E2E_BRIDGE</c>
/// set) — the harness spawns/kills instances directly and must not be OS-relaunched.
/// Mirrors <see cref="SingleInstanceManager"/>.</para>
/// </summary>
internal static class RestartManagerService
{
    private const string WindowClassName = "FaunaLifecycleWindow";

    // Top-level (parent = NULL), never shown: WS_POPUP (no caption/border) +
    // WS_EX_TOOLWINDOW (out of alt-tab / taskbar). It exists only to receive
    // session-end messages.
    private const uint WS_POPUP = 0x80000000;
    private const uint WS_EX_TOOLWINDOW = 0x00000080;

    private static WndProcDelegate? _wndProcDelegate; // prevent GC of the delegate
    private static IntPtr _lifecycleWindow;
    private static bool _initialized;

    /// <summary>
    /// True once the OS has told us the session is ending (a
    /// <c>WM_QUERYENDSESSION</c> arrived). Read by the <see cref="TrayIconService"/>
    /// close handler so a session-end is never turned into a tray-hide even when
    /// Close-to-tray is on.
    /// </summary>
    internal static bool SessionEnding { get; private set; }

    // Via E2eEnv so the read is compiled out of release builds (convention 15);
    // the production twin returns null, so IsE2E is false exactly as before and
    // restart-manager registration stays on in the shipped app.
    private static bool IsE2E => E2eEnv.Bridge is not null;

    private delegate IntPtr WndProcDelegate(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WNDCLASSW
    {
        public uint style;
        public IntPtr lpfnWndProc;
        public int cbClsExtra;
        public int cbWndExtra;
        public IntPtr hInstance;
        public IntPtr hIcon;
        public IntPtr hCursor;
        public IntPtr hbrBackground;
        [MarshalAs(UnmanagedType.LPWStr)]
        public string? lpszMenuName;
        [MarshalAs(UnmanagedType.LPWStr)]
        public string lpszClassName;
    }

    // dwFlags = 0 → restart in all cases (the installer-upgrade / reboot path we
    // care about is covered; a future session can mask out crash/hang restarts via
    // RESTART_NO_CRASH | RESTART_NO_HANG if a crash-loop relaunch is undesirable).
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern int RegisterApplicationRestart(string? pwzCommandline, uint dwFlags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern ushort RegisterClassW(ref WNDCLASSW lpWndClass);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateWindowExW(
        uint dwExStyle, string lpClassName, string lpWindowName, uint dwStyle,
        int x, int y, int nWidth, int nHeight,
        IntPtr hWndParent, IntPtr hMenu, IntPtr hInstance, IntPtr lpParam);

    [DllImport("user32.dll")]
    private static extern IntPtr DefWindowProcW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DestroyWindow(IntPtr hWnd);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetModuleHandleW(IntPtr lpModuleName);

    /// <summary>
    /// Register for OS restart and start listening for session-end. Call once at
    /// startup after the main window exists (so the graceful-quit teardown via
    /// <see cref="TrayIconService"/> has a window to close). No-op under E2E.
    /// Best-effort: any failure is swallowed — cooperative shutdown degrades to the
    /// installer's force-kill net, which must never block launch.
    /// </summary>
    public static void Initialize()
    {
        if (_initialized) return;
        if (IsE2E || !RestartManagerGate.ShouldRegisterRestart(IsE2E)) return;

        try
        {
            // Relaunch with the original command line (pwzCommandline = null) after
            // an RM-driven shutdown — a clean user launch has no args to preserve.
            RegisterApplicationRestart(null, 0);
            CreateLifecycleWindow();
            _initialized = true;
        }
        catch
        {
            // Cooperative shutdown is best-effort; the installer's taskkill CA is the net.
        }
    }

    private static void CreateLifecycleWindow()
    {
        _wndProcDelegate = WndProc;
        var hInstance = GetModuleHandleW(IntPtr.Zero);

        var wndClass = new WNDCLASSW
        {
            lpfnWndProc = Marshal.GetFunctionPointerForDelegate(_wndProcDelegate),
            hInstance = hInstance,
            lpszClassName = WindowClassName,
        };

        var atom = RegisterClassW(ref wndClass);
        if (atom == 0) return;

        // Top-level (HWND parent = IntPtr.Zero), zero size, never ShowWindow'd → it
        // is invisible but still a top-level window, so it receives session-end.
        _lifecycleWindow = CreateWindowExW(
            WS_EX_TOOLWINDOW, WindowClassName, "Fauna Lifecycle", WS_POPUP,
            0, 0, 0, 0,
            IntPtr.Zero, IntPtr.Zero, hInstance, IntPtr.Zero);
    }

    /// <summary>
    /// Tear down the lifecycle window. Called from the graceful-quit teardown so the
    /// hidden window doesn't outlive the process's exit path.
    /// </summary>
    internal static void Shutdown()
    {
        try
        {
            if (_lifecycleWindow != IntPtr.Zero)
            {
                DestroyWindow(_lifecycleWindow);
                _lifecycleWindow = IntPtr.Zero;
            }
        }
        catch { }
        _initialized = false;
    }

    private static IntPtr WndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        if (RestartManagerGate.Classify(msg) == ShutdownClassification.ShutdownRequest)
        {
            if (msg == RestartManagerGate.WmQueryEndSession)
            {
                // The session is being asked to end. Save drafts NOW (before any
                // teardown — the system may terminate us abruptly after WM_ENDSESSION)
                // and agree to the shutdown (return TRUE). Vetoing would make the
                // installer/RM force-kill us — the opposite of cooperative.
                SessionEnding = true;
                // Both draft rails are nest-backed (draft-persistence v2): flush the
                // latest (already live-forwarded into the manager) to the nest before
                // the system may terminate us. Bounded; SaveNowAsync is
                // ConfigureAwait(false) so the wait can't deadlock the message-pump
                // thread. Best-effort.
                try { FaunaApp.App.ConvDrafts?.SaveNowAsync().Wait(1500); } catch { }
                try { FaunaApp.App.FeedDrafts?.FlushIfPendingAsync().Wait(1500); } catch { }
                try { FaunaApp.App.EventDrafts?.FlushIfPendingAsync().Wait(1500); } catch { }
                return new IntPtr(1); // TRUE — allow the session to end
            }

            // WM_ENDSESSION: wParam != 0 means the session is really ending. Quit
            // gracefully (persist again — idempotent — remove the tray icon, close
            // the main window, exit) so RM sees a clean exit and relaunches us.
            if (wParam != IntPtr.Zero)
            {
                TrayIconService.QuitApplication();
            }
            return IntPtr.Zero;
        }

        return DefWindowProcW(hWnd, msg, wParam, lParam);
    }
}
