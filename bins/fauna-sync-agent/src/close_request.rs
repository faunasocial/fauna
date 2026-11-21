//! The agent's answer to the system's request to close — Windows
//! (`docs/goal/architecture/apps/sync-agent.md` § Packaging + lifecycle, the windows
//! bullet; `docs/goal/architecture/installers/windows.md` § Direct download, *2. Updates*).
//!
//! A deployment over a running package — an update with `-ForceApplicationShutdown`,
//! App Installer's close-and-update, a removal — asks each of the package's processes
//! to close by sending its top-level windows the session-end messages, and the system
//! does the same at logoff. The shipped agent is a GUI-subsystem process with no
//! console, and before this module it owned no window, so no close request could reach
//! it: its run loop waited on ctrl-c (a console event) or its IPC server alone, Windows
//! waited out its timeout, ended it and filed a `MoAppHang` of hang type `Quiesce`
//! against the package — on every update the install witness made (measured 2026-11-16
//! and 2026-11-17, six reports, every one naming `fauna-sync-agent.exe`).
//!
//! So the agent owns one hidden, never-shown TOP-LEVEL window on a thread of its own
//! with its own message pump. Top-level, because a message-only window (`HWND_MESSAGE`)
//! never receives session-end messages; the app's `RestartManagerService` lifecycle
//! window is the same shape (`WS_POPUP`, `WS_EX_TOOLWINDOW`, zero size, never shown).
//!
//! - `WM_QUERYENDSESSION` → agree (TRUE). Nothing is torn down yet: the query may still
//!   be cancelled.
//! - `WM_ENDSESSION` with wParam TRUE, or `WM_CLOSE` → wake the run loop's close arm
//!   ([`CloseListener::requested`]), which takes the orderly shutdown path ctrl-c takes,
//!   and hold the message until the run is finished — the system may end the process as
//!   soon as `WM_ENDSESSION` returns. The hold is bounded by [`CLOSE_EXIT_BUDGET`]: past
//!   it the listener exits the process itself with [`CLOSE_OVERRUN_EXIT_CODE`], so a
//!   teardown that wedges can never turn a close into a hang report.
//! - `WM_ENDSESSION` with wParam FALSE → the end was cancelled; the agent stays up.
//!
//! The listener is started by `run_main` before the runtime and dropped after it, so its
//! bound covers the whole teardown, the runtime's included. Dropping it releases a held
//! close message and ends the thread. Best-effort: a listener that cannot start logs why
//! and the agent runs without it, as it did before.

use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use windows::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
    PostThreadMessageW, RegisterClassW, TranslateMessage, WM_CLOSE, WM_ENDSESSION,
    WM_QUERYENDSESSION, WM_QUIT, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{PCWSTR, w};

/// How long a close request may take, from the message to the process's exit, before
/// the listener ends the process itself. Under the system's own patience for a closing
/// process, with room to spare; the orderly teardown of an agent with no capability
/// takes milliseconds.
pub(crate) const CLOSE_EXIT_BUDGET: Duration = Duration::from_secs(3);

/// The exit code of a close whose orderly teardown overran [`CLOSE_EXIT_BUDGET`]: the
/// process still closed as asked, inside the bound, but not by the run loop's path. An
/// orderly close exits 0. Distinct so a witness can tell the two apart from the exit
/// code alone (`tests/agent_close_request.rs`).
pub(crate) const CLOSE_OVERRUN_EXIT_CODE: i32 = 2;

const CLASS_NAME: PCWSTR = w!("FaunaSyncAgentLifecycleWindow");

/// What the window's thread and the run loop share.
struct Shared {
    /// Woken by a close request; a request that comes before the run loop waits is
    /// kept as `Notify`'s permit.
    requested: tokio::sync::Notify,
    /// Set when the listener is dropped — the run, and the runtime, are finished.
    finished: Mutex<bool>,
    finished_changed: Condvar,
}

// The one listener's state, for the window procedure, which runs on the listener's
// own thread and takes no context of ours.
thread_local! {
    static LISTENER: OnceLock<Arc<Shared>> = const { OnceLock::new() };
}

/// The running listener. Hold it for the whole run; dropping it releases a held close
/// message and ends the window's thread.
pub(crate) struct CloseListener {
    shared: Arc<Shared>,
    thread_id: u32,
}

impl CloseListener {
    /// Start the window's thread and wait until its window exists. `None` (logged)
    /// when it cannot: the agent then runs as it did before it had one.
    pub(crate) fn start() -> Option<Self> {
        let shared = Arc::new(Shared {
            requested: tokio::sync::Notify::new(),
            finished: Mutex::new(false),
            finished_changed: Condvar::new(),
        });
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("close-request".into())
            .spawn(move || pump(thread_shared, ready_tx));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "close listener: could not start its thread — the system's close request will not reach the agent");
            return None;
        }
        match ready_rx.recv() {
            Ok(Some(thread_id)) => Some(Self { shared, thread_id }),
            // The thread logged why it could not make its window.
            Ok(None) | Err(_) => None,
        }
    }

    /// Resolves once the system has asked the agent to close.
    pub(crate) async fn requested(&self) {
        self.shared.requested.notified().await;
    }
}

/// Resolves once the system has asked the agent to close — never, with no listener
/// (one that could not start, or a run with none).
pub(crate) async fn requested(listener: Option<&CloseListener>) {
    match listener {
        Some(listener) => listener.requested().await,
        None => std::future::pending().await,
    }
}

impl Drop for CloseListener {
    fn drop(&mut self) {
        *self
            .shared
            .finished
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        self.shared.finished_changed.notify_all();
        // The thread made its window, and with it its message queue, before it said it
        // was ready, so the post has a queue to land in. A failure means the thread is
        // already gone.
        // SAFETY: posts a message to a thread id; no pointers are passed.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
}

/// The window's thread: make the window, say so, pump until `WM_QUIT`.
fn pump(shared: Arc<Shared>, ready: mpsc::Sender<Option<u32>>) {
    LISTENER.with(|slot| {
        let _ = slot.set(shared);
    });
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            tracing::warn!(error = %e, "close listener: could not create its window — the system's close request will not reach the agent");
            let _ = ready.send(None);
            return;
        }
    };
    // SAFETY: reads the calling thread's id.
    let _ = ready.send(Some(unsafe { GetCurrentThreadId() }));
    tracing::debug!("close listener: listening for the system's close request");

    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out-pointer for the call; a `None` window takes every
        // message of this thread, the posted `WM_QUIT` included.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        // 0 is `WM_QUIT`, -1 an error: either way the pump is over.
        if got.0 == 0 || got.0 == -1 {
            break;
        }
        // SAFETY: `msg` was just filled by `GetMessageW`.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // SAFETY: the window belongs to this thread, which destroys it.
    let _ = unsafe { DestroyWindow(hwnd) };
}

fn create_window() -> windows::core::Result<HWND> {
    // SAFETY: a null name asks for this process's own module handle.
    let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialized and its strings are static. A class is the
    // process's: a second listener in one process (an in-process test running the loop
    // twice) finds it registered already, with this same procedure.
    if unsafe { RegisterClassW(&class) } == 0 {
        let e = windows::core::Error::from_win32();
        if e.code() != ERROR_CLASS_ALREADY_EXISTS.to_hresult() {
            return Err(e);
        }
    }
    // Top-level (no parent), zero size, never shown: invisible, out of the taskbar and
    // Alt-Tab, and still sent the session-end messages.
    // SAFETY: the class is registered above and the strings are static.
    unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("Fauna sync agent"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => {
            tracing::info!(
                flags = format_args!("{:#x}", lparam.0),
                "the system asks whether the agent may close (WM_QUERYENDSESSION) — agreeing"
            );
            LRESULT(1)
        }
        WM_ENDSESSION if wparam.0 != 0 => {
            close_now("WM_ENDSESSION", lparam.0);
            LRESULT(0)
        }
        WM_ENDSESSION => {
            tracing::info!("the system's close was cancelled (WM_ENDSESSION, FALSE) — staying up");
            LRESULT(0)
        }
        // Answered here rather than by the default procedure, which would destroy the
        // window and leave the agent running with no way to hear the next request.
        WM_CLOSE => {
            close_now("WM_CLOSE", 0);
            LRESULT(0)
        }
        // SAFETY: the arguments are the ones this procedure was called with.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Wake the run loop's close arm and hold the message until the run has finished, at
/// most [`CLOSE_EXIT_BUDGET`]; past it, exit the process.
fn close_now(message: &str, flags: isize) {
    let Some(shared) = LISTENER.with(|slot| slot.get().cloned()) else {
        return;
    };
    tracing::info!(
        message,
        flags = format_args!("{flags:#x}"),
        "the system asked the agent to close"
    );
    shared.requested.notify_one();
    let finished = shared
        .finished
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let (finished, _) = shared
        .finished_changed
        .wait_timeout_while(finished, CLOSE_EXIT_BUDGET, |done| !*done)
        .unwrap_or_else(PoisonError::into_inner);
    if !*finished {
        tracing::warn!(
            budget = ?CLOSE_EXIT_BUDGET,
            "the orderly close did not finish in its budget — exiting now"
        );
        std::process::exit(CLOSE_OVERRUN_EXIT_CODE);
    }
}
