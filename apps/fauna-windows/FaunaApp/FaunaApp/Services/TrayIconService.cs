using System;
using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;
using FaunaApp.Core;
using FaunaApp.Core.Services;

namespace FaunaApp.Services;

/// <summary>
/// System tray icon using Win32 Shell_NotifyIconW P/Invoke.
/// WinUI 3 has no built-in tray API, so we create a hidden message-only window
/// to receive tray icon callbacks, and use Shell_NotifyIconW to manage the icon.
/// </summary>
public static class TrayIconService
{
    private static Window? _mainWindow;
    private static IntPtr _messageWindowHandle;
    private static bool _initialized;
    private static WndProcDelegate? _wndProcDelegate; // prevent GC of delegate

    // Custom window message for tray icon callbacks
    private const uint WM_TRAYICON = 0x8000; // WM_APP
    private const uint TRAY_ICON_ID = 1;

    // Window messages
    private const uint WM_DESTROY = 0x0002;
    private const uint WM_COMMAND = 0x0111;
    private const uint WM_LBUTTONDBLCLK = 0x0203;
    private const uint WM_RBUTTONUP = 0x0205;

    // Shell_NotifyIcon messages
    private const uint NIM_ADD = 0x00000000;
    private const uint NIM_DELETE = 0x00000002;

    // NOTIFYICONDATA flags
    private const uint NIF_MESSAGE = 0x00000001;
    private const uint NIF_ICON = 0x00000002;
    private const uint NIF_TIP = 0x00000004;

    // ShowWindow constants
    private const int SW_HIDE = 0;
    private const int SW_SHOW = 5;
    private const int SW_RESTORE = 9;

    // Menu constants
    private const uint MF_STRING = 0x00000000;
    private const uint TPM_RETURNCMD = 0x0100;
    private const uint TPM_NONOTIFY = 0x0080;

    // Menu item IDs
    private const int IDM_SHOW = 1001;
    private const int IDM_QUIT = 1002;

    // LoadIcon standard IDs
    private static readonly IntPtr IDI_APPLICATION = new IntPtr(32512);

    // Window class style
    private const uint CS_HREDRAW = 0x0002;
    private const uint CS_VREDRAW = 0x0001;

    // HWND_MESSAGE for message-only window
    private static readonly IntPtr HWND_MESSAGE = new IntPtr(-3);

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

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NOTIFYICONDATAW
    {
        public uint cbSize;
        public IntPtr hWnd;
        public uint uID;
        public uint uFlags;
        public uint uCallbackMessage;
        public IntPtr hIcon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)]
        public string szTip;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT
    {
        public int X;
        public int Y;
    }

    // P/Invoke declarations
    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool Shell_NotifyIconW(uint dwMessage, ref NOTIFYICONDATAW lpData);

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

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr LoadIconW(IntPtr hInstance, IntPtr lpIconName);

    [DllImport("user32.dll")]
    private static extern IntPtr CreatePopupMenu();

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool InsertMenuW(IntPtr hMenu, uint uPosition, uint uFlags, IntPtr uIDNewItem, string lpNewItem);

    [DllImport("user32.dll")]
    private static extern int TrackPopupMenu(IntPtr hMenu, uint uFlags, int x, int y, int nReserved, IntPtr hWnd, IntPtr prcRect);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DestroyMenu(IntPtr hMenu);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetCursorPos(out POINT lpPoint);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetModuleHandleW(IntPtr lpModuleName);

    /// <summary>
    /// Initialize the tray icon. Call once after the main window is created and activated.
    /// </summary>
    public static void Initialize(Window mainWindow)
    {
        if (_initialized) return;

        _mainWindow = mainWindow;

        try
        {
            CreateMessageWindow();
            AddTrayIcon();
            _initialized = true;

            // Window-close behaviour follows the user's "Close to tray" setting
            // (apps/windows.md § App Lifecycle). ON (the default since the 2026-07-16
            // shape-A residency ratification — closing a window must not silently stop
            // file sync / badges / toasts; the deliberate stop is the tray Quit) ⇒ hide
            // to the tray, staying resident; OFF (an explicit opt-out) ⇒ quit — but
            // first persist unsaved compose input (principle 2 — "no close path leaves
            // unsaved state") and tear down, so no windowless FaunaApp lingers.
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(_mainWindow);
            var windowId = Microsoft.UI.Win32Interop.GetWindowIdFromWindow(hwnd);
            var appWindow = Microsoft.UI.Windowing.AppWindow.GetFromWindowId(windowId);
            appWindow.Closing += (s, e) =>
            {
                // A session-end / Restart-Manager shutdown must never be turned into
                // a tray-hide, even when Close-to-tray is on — the OS/installer is
                // taking the app down, so persist-and-quit. (AppWindow.Closing is
                // WM_CLOSE-driven and does not normally fire on session-end, which
                // RestartManagerService handles via WM_QUERYENDSESSION/WM_ENDSESSION;
                // this is defence-in-depth.)
                if (!RestartManagerService.SessionEnding && new AppSettingsStore().CloseToTray)
                {
                    e.Cancel = true;
                    ShowWindow(hwnd, SW_HIDE);
                }
                else
                {
                    QuitApplication();
                }
            };
        }
        catch
        {
            // Tray icon is non-critical; app works without it
        }
    }

    /// <summary>
    /// Remove the tray icon and clean up. Call on true app exit.
    /// </summary>
    public static void Shutdown()
    {
        if (!_initialized) return;

        try
        {
            RemoveTrayIcon();
            if (_messageWindowHandle != IntPtr.Zero)
            {
                DestroyWindow(_messageWindowHandle);
                _messageWindowHandle = IntPtr.Zero;
            }
        }
        catch { }

        _initialized = false;
    }

    private static void CreateMessageWindow()
    {
        _wndProcDelegate = WndProc;
        var hInstance = GetModuleHandleW(IntPtr.Zero);

        var wndClass = new WNDCLASSW
        {
            style = CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc = Marshal.GetFunctionPointerForDelegate(_wndProcDelegate),
            hInstance = hInstance,
            lpszClassName = "FaunaTrayIconWindow",
        };

        var atom = RegisterClassW(ref wndClass);
        if (atom == 0) return;

        _messageWindowHandle = CreateWindowExW(
            0, "FaunaTrayIconWindow", "Fauna Tray", 0,
            0, 0, 0, 0,
            HWND_MESSAGE, IntPtr.Zero, hInstance, IntPtr.Zero);
    }

    private static void AddTrayIcon()
    {
        if (_messageWindowHandle == IntPtr.Zero) return;

        var hIcon = LoadIconW(IntPtr.Zero, IDI_APPLICATION);

        var nid = new NOTIFYICONDATAW
        {
            cbSize = (uint)Marshal.SizeOf<NOTIFYICONDATAW>(),
            hWnd = _messageWindowHandle,
            uID = TRAY_ICON_ID,
            uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage = WM_TRAYICON,
            hIcon = hIcon,
            szTip = "Fauna",
        };

        Shell_NotifyIconW(NIM_ADD, ref nid);
    }

    private static void RemoveTrayIcon()
    {
        if (_messageWindowHandle == IntPtr.Zero) return;

        var nid = new NOTIFYICONDATAW
        {
            cbSize = (uint)Marshal.SizeOf<NOTIFYICONDATAW>(),
            hWnd = _messageWindowHandle,
            uID = TRAY_ICON_ID,
        };

        Shell_NotifyIconW(NIM_DELETE, ref nid);
    }

    private static IntPtr WndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        if (msg == WM_TRAYICON)
        {
            var mouseMsg = (uint)(lParam.ToInt64() & 0xFFFF);

            if (mouseMsg == WM_LBUTTONDBLCLK)
            {
                ShowMainWindow();
            }
            else if (mouseMsg == WM_RBUTTONUP)
            {
                ShowContextMenu();
            }

            return IntPtr.Zero;
        }

        if (msg == WM_COMMAND)
        {
            var menuId = (int)(wParam.ToInt64() & 0xFFFF);
            switch (menuId)
            {
                case IDM_SHOW:
                    ShowMainWindow();
                    break;
                case IDM_QUIT:
                    QuitApplication();
                    break;
            }
            return IntPtr.Zero;
        }

        return DefWindowProcW(hWnd, msg, wParam, lParam);
    }

    /// <summary>
    /// Restore + foreground the main window (un-hides it when minimized to tray).
    /// Reused by <see cref="SingleInstanceManager"/> to surface the primary when a
    /// second launch is redirected.
    /// </summary>
    internal static void ShowMainWindow()
    {
        if (_mainWindow is null) return;

        try
        {
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(_mainWindow);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
        catch { }
    }

    private static void ShowContextMenu()
    {
        var hMenu = CreatePopupMenu();
        if (hMenu == IntPtr.Zero) return;

        try
        {
            InsertMenuW(hMenu, 0, MF_STRING, new IntPtr(IDM_SHOW), $"{Strings.Get("common/open")} Fauna");
            InsertMenuW(hMenu, 1, MF_STRING, new IntPtr(IDM_QUIT), Strings.Get("common/quit"));

            GetCursorPos(out var pt);

            // SetForegroundWindow is required before TrackPopupMenu so the menu dismisses properly
            SetForegroundWindow(_messageWindowHandle);

            int cmd = TrackPopupMenu(hMenu, TPM_RETURNCMD | TPM_NONOTIFY, pt.X, pt.Y, 0, _messageWindowHandle, IntPtr.Zero);
            if (cmd == IDM_SHOW)
            {
                ShowMainWindow();
            }
            else if (cmd == IDM_QUIT)
            {
                QuitApplication();
            }
        }
        finally
        {
            DestroyMenu(hMenu);
        }
    }

    /// <summary>
    /// Persist unsaved drafts, remove the tray icon, close the main window, and exit.
    /// The single graceful-quit path: the tray Quit menu, a close when Close-to-tray
    /// is off, and the Restart-Manager session-end (<see cref="RestartManagerService"/>)
    /// all funnel through here.
    /// </summary>
    internal static void QuitApplication()
    {
        // Both draft rails are nest-backed (draft-persistence v2): the live
        // manager holds the latest compose body (live-forwarded on every edit),
        // so flush before exit so a quit-within-the-debounce-window doesn't lose
        // it (apps/windows.md § App Lifecycle, principle 2 — "no close path
        // leaves unsaved state"). Bounded so a dead connection can't hang the
        // quit; SaveNowAsync is ConfigureAwait(false) throughout, so .Wait can't
        // deadlock the UI thread. Best-effort.
        try { FaunaApp.App.ConvDrafts?.SaveNowAsync().Wait(1500); } catch { }
        try { FaunaApp.App.FeedDrafts?.FlushIfPendingAsync().Wait(1500); } catch { }
        try { FaunaApp.App.EventDrafts?.FlushIfPendingAsync().Wait(1500); } catch { }
        // Force a put of any unsaved engagement-cue rollup (engagement-cues.md
        // § At rest; task-6 of the personalization-port plan) — same best-effort,
        // swallow-on-failure shape as the two flushes above: a failed flush just
        // leaves the rollup dirty for the next session's debounced put, and
        // there's no UI left at quit time to surface an error to.
        // FeedManagerHost is the layer-below-both seam (see its own doc
        // comment) — TrayIconService has no page/VM reference of its own.
        try { FeedManagerHost.Current?.FlushCues().Wait(1500); } catch { }

        Shutdown();
        // Tear down the hidden session-end listener window too, so no top-level
        // window outlives the exit path.
        try { RestartManagerService.Shutdown(); } catch { }

        // Actually close the main window (bypass the Closing cancel handler)
        if (_mainWindow is not null)
        {
            try
            {
                var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(_mainWindow);
                var windowId = Microsoft.UI.Win32Interop.GetWindowIdFromWindow(hwnd);
                var appWindow = Microsoft.UI.Windowing.AppWindow.GetFromWindowId(windowId);

                // Remove our closing handler by destroying directly
                _mainWindow = null;
                DestroyWindow(hwnd);
            }
            catch { }
        }

        Environment.Exit(0);
    }
}
