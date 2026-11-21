//! The agent **answers the system's request to close** — Windows; the real binary,
//! real window messages, a real exit code (`docs/goal/architecture/apps/sync-agent.md`
//! § Packaging + lifecycle, the windows bullet; `installers/windows.md` § Direct
//! download, *2. Updates*). The windows twin of `agent_sigterm.rs`.
//!
//! ## Why this test exists
//!
//! A deployment over a running package — an update with `-ForceApplicationShutdown`,
//! App Installer's close-and-update — asks each of the package's processes to close
//! through its top-level windows (`WM_QUERYENDSESSION`, then `WM_ENDSESSION`). The
//! shipped agent is a GUI-subsystem process with no console, and until it owned a
//! window no close request could reach it: Windows waited out its timeout, ended the
//! agent and filed a `MoAppHang` of hang type `Quiesce` against the package on every
//! update the install witness made (measured 2026-11-16 and 2026-11-17).
//!
//! ## The discriminators
//!
//! - **The agent owns a top-level window.** A build without the listener owns none, and
//!   the test fails there, naming that.
//! - **It agrees to `WM_QUERYENDSESSION`** (the message returns TRUE).
//! - **After `WM_ENDSESSION` — or `WM_CLOSE` — the process exits with code 0**: the run
//!   loop's orderly path. Not `2`, the listener's own exit when the orderly teardown
//!   overruns its budget (`close_request::CLOSE_OVERRUN_EXIT_CODE`), and not a kill
//!   (`KillOnDrop`'s `TerminateProcess` exits 1 and only runs once the test has failed).
//!
//! The test sends the messages itself, as the deployment stack does. Which messages a
//! real deployment sends, and that it then files no hang, is the install witness's
//! measurement (`tests/e2e-unified/tests/real_session/test_direct_package_install_witness.py`);
//! this proves the agent hears a close request and closes in order.
//!
//! ## Isolation + timing
//!
//! Its own pipe name per test (so its own instance mutex, which is derived from the
//! pipe name), `--data-dir`, credential directory and `LOCALAPPDATA`; no capability is
//! provisioned, so the agent reaches no nest and no keyring. Every wait is a deadline
//! poll on a generous budget, never a sleep-as-assertion (testing.md convention 14).

#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use fauna_ipc::sync::RequestMethod;
use fauna_ipc::sync_pipe_client::SyncPipeClient;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    ENDSESSION_CLOSEAPP, EnumWindows, GetWindowThreadProcessId, SMTO_ABORTIFHUNG,
    SendMessageTimeoutW, WM_CLOSE, WM_ENDSESSION, WM_QUERYENDSESSION,
};
use windows::core::BOOL;

/// From spawn to a served pipe: a cold debug agent's boot reconcile included.
const BOOT_BUDGET: Duration = Duration::from_secs(120);
/// Far above the listener's own 3 s bound; a green run never approaches it.
const EXIT_BUDGET: Duration = Duration::from_secs(30);
/// How long one sent message may take to be answered. `WM_ENDSESSION`'s handler
/// holds the message until the agent's run has finished, so this is above that too.
const SEND_TIMEOUT_MS: u32 = 15_000;

/// A spawned agent that cannot outlive the test, however the test ends
/// (`std::process::Child` does not kill on drop — `agent_sigterm.rs`'s lesson).
struct KillOnDrop(Option<Child>);

impl KillOnDrop {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("child still owned")
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One agent in a private world, its stderr (where its logs go too) in a file the
/// failure messages quote.
struct Agent {
    process: KillOnDrop,
    pipe: String,
    stderr_log: PathBuf,
    _world: tempfile::TempDir,
}

impl Agent {
    fn spawn(label: &str) -> Self {
        let world = tempfile::Builder::new()
            .prefix(&format!("fauna-agent-close-{label}-"))
            .tempdir()
            .unwrap();
        let root = world.path();
        let local = root.join("local");
        std::fs::create_dir_all(&local).unwrap();
        let stderr_log = root.join("agent-stderr.log");
        let pipe = format!(
            r"\\.\pipe\fauna-sync-close-request-{label}-{}",
            std::process::id()
        );
        let child = Command::new(env!("CARGO_BIN_EXE_fauna-sync-agent"))
            .arg("--pipe-name")
            .arg(&pipe)
            .arg("--data-dir")
            .arg(root.join("data"))
            .env("FAUNA_E2E_CREDENTIAL_DIR", root.join("creds"))
            .env("LOCALAPPDATA", &local)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr_log).unwrap())
            .spawn()
            .expect("spawn fauna-sync-agent");
        Self {
            process: KillOnDrop(Some(child)),
            pipe,
            stderr_log,
            _world: world,
        }
    }

    fn pid(&mut self) -> u32 {
        self.process.child().id()
    }

    fn log_tail(&self) -> String {
        let text = std::fs::read_to_string(&self.stderr_log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(30)..].join("\n")
    }

    fn exited(&mut self) -> Option<ExitStatus> {
        self.process
            .child()
            .try_wait()
            .expect("try_wait must not error")
    }

    /// Wait until the agent answers a status request on its pipe — booted, its
    /// run loop at the shutdown select.
    fn await_serving(&mut self) {
        let deadline = Instant::now() + BOOT_BUDGET;
        loop {
            if let Some(status) = self.exited() {
                panic!(
                    "the agent exited ({status}) before serving {}. Its log:\n{}",
                    self.pipe,
                    self.log_tail()
                );
            }
            if let Ok(client) = SyncPipeClient::connect_pipe_to(&self.pipe)
                && client
                    .request_with_timeout(RequestMethod::GetServiceStatus, Duration::from_secs(10))
                    .is_ok()
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the agent did not serve {} within {BOOT_BUDGET:?}. Its log:\n{}",
                self.pipe,
                self.log_tail()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The agent's top-level windows, hidden ones included (`EnumWindows` lists every
    /// top-level window, shown or not).
    fn top_level_windows(&mut self) -> Vec<HWND> {
        let pid = self.pid();
        let mut found = Found {
            pid,
            windows: Vec::new(),
        };
        // SAFETY: the callback only reads its two arguments and pushes into `found`,
        // which outlives the synchronous `EnumWindows` call.
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut found as *mut Found as isize));
        }
        found.windows
    }

    /// Wait for the agent to exit and return its exit code.
    fn await_exit_code(&mut self, after: &str) -> u32 {
        let deadline = Instant::now() + EXIT_BUDGET;
        loop {
            if let Some(status) = self.exited() {
                return status.code().expect("a windows exit status carries a code") as u32;
            }
            assert!(
                Instant::now() < deadline,
                "the agent did not exit within {EXIT_BUDGET:?} of {after}. Its log:\n{}",
                self.log_tail()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

struct Found {
    pid: u32,
    windows: Vec<HWND>,
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Found` `top_level_windows` passed, alive for the
    // whole enumeration.
    let found = unsafe { &mut *(lparam.0 as *mut Found) };
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner as *mut u32)) };
    if owner == found.pid {
        found.windows.push(hwnd);
    }
    BOOL(1)
}

/// Send `msg` and return what the window procedure answered, or `None` when the send
/// itself failed or timed out (a process that exits while the send is pending ends it
/// that way too).
fn send(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> Option<usize> {
    let mut answer = 0usize;
    // SAFETY: a plain cross-process send to a window handle; a stale handle fails the
    // call, it does not fault.
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            msg,
            WPARAM(wparam),
            LPARAM(lparam),
            SMTO_ABORTIFHUNG,
            SEND_TIMEOUT_MS,
            Some(&mut answer as *mut usize),
        )
    };
    (sent.0 != 0).then_some(answer)
}

/// The agent's one window, asserting it has one: the discriminator a build with no
/// close listener fails.
fn the_window(agent: &mut Agent) -> HWND {
    let windows = agent.top_level_windows();
    assert!(
        !windows.is_empty(),
        "the agent (pid {}) owns no top-level window, so no close request a deployment \
         sends can reach it — Windows would end it and file a MoAppHang (Quiesce). Its log:\n{}",
        agent.pid(),
        agent.log_tail()
    );
    windows[0]
}

fn assert_orderly(code: u32, after: &str, agent: &Agent) {
    assert_eq!(
        code,
        0,
        "the agent exited with {code:#x} after {after}, not 0 — 2 is the close listener's \
         own exit when the orderly teardown overran its budget. Its log:\n{}",
        agent.log_tail()
    );
}

#[test]
fn the_agent_binary_closes_in_order_when_the_session_ends() {
    let mut agent = Agent::spawn("endsession");
    agent.await_serving();
    let hwnd = the_window(&mut agent);

    let agreed = send(hwnd, WM_QUERYENDSESSION, 0, ENDSESSION_CLOSEAPP as isize);
    assert_eq!(
        agreed,
        Some(1),
        "WM_QUERYENDSESSION was not answered TRUE. Its log:\n{}",
        agent.log_tail()
    );
    assert!(
        agent.exited().is_none(),
        "the agent exited on WM_QUERYENDSESSION alone — a query may still be cancelled"
    );

    // The handler holds WM_ENDSESSION until the run has finished, so the send returns
    // as the process goes; its answer is not the assertion.
    let _ = send(hwnd, WM_ENDSESSION, 1, ENDSESSION_CLOSEAPP as isize);
    let code = agent.await_exit_code("WM_ENDSESSION");
    assert_orderly(code, "WM_ENDSESSION", &agent);
}

#[test]
fn the_agent_binary_closes_in_order_on_wm_close() {
    let mut agent = Agent::spawn("wmclose");
    agent.await_serving();
    let hwnd = the_window(&mut agent);

    let _ = send(hwnd, WM_CLOSE, 0, 0);
    let code = agent.await_exit_code("WM_CLOSE");
    assert_orderly(code, "WM_CLOSE", &agent);
}

#[test]
fn a_cancelled_session_end_leaves_the_agent_serving() {
    let mut agent = Agent::spawn("cancelled");
    agent.await_serving();
    let hwnd = the_window(&mut agent);

    // WM_ENDSESSION with wParam FALSE: another process vetoed the session's end.
    assert_eq!(send(hwnd, WM_QUERYENDSESSION, 0, 0), Some(1));
    assert!(send(hwnd, WM_ENDSESSION, 0, 0).is_some());
    // Still serving: a request round-trips after the cancelled end.
    let client = SyncPipeClient::connect_pipe_to(&agent.pipe).unwrap_or_else(|e| {
        panic!(
            "the agent stopped serving after a cancelled session end ({e}). Its log:\n{}",
            agent.log_tail()
        )
    });
    client
        .request_with_timeout(RequestMethod::GetServiceStatus, Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "the agent did not answer after a cancelled session end ({e}). Its log:\n{}",
                agent.log_tail()
            )
        });
    assert!(
        agent.exited().is_none(),
        "the agent exited on a cancelled session end"
    );
    drop(client);

    // And it still closes when asked for real.
    let _ = send(hwnd, WM_CLOSE, 0, 0);
    let code = agent.await_exit_code("WM_CLOSE after a cancelled session end");
    assert_orderly(code, "WM_CLOSE after a cancelled session end", &agent);
}
