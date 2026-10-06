use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Signal handling
// ---------------------------------------------------------------------------

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
}

extern "C" fn signal_handler(_sig: i32) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

const DEFAULT_BIND: &str = "127.0.0.1:7370";
const DEFAULT_AVD: &str = "fauna-test";
const MAX_REQUEST_BYTES: usize = 8192;
const MAX_LOG_LINES: usize = 1000;

fn default_token_file() -> PathBuf {
    let mut p = dirs_home();
    p.push(".config/fauna-emulator-ctl/token");
    p
}

fn default_android_home() -> PathBuf {
    let mut p = dirs_home();
    p.push("android-sdk");
    p
}

/// Returns the user's home directory, or panics with a clear message.
fn dirs_home() -> PathBuf {
    // std-only: try HOME env var (Unix), then USERPROFILE (Windows)
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .expect("cannot determine home directory: neither HOME nor USERPROFILE is set")
}

fn print_usage(program: &str) {
    eprintln!("Usage: {program} [OPTIONS]");
    eprintln!();
    eprintln!("Minimal HTTP service to control the Android emulator on bare-metal macOS.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --bind <ADDR>         Socket address to listen on [default: {DEFAULT_BIND}]");
    eprintln!("                        Note: 0.0.0.0 is rejected for security reasons.");
    eprintln!("  --token-file <PATH>   Path to the bearer-token file");
    eprintln!("                        [default: ~/.config/fauna-emulator-ctl/token]");
    eprintln!("  --android-home <PATH> Android SDK root [default: ~/android-sdk]");
    eprintln!("  --avd <NAME>          Android Virtual Device name [default: {DEFAULT_AVD}]");
    eprintln!("  --help                Print this help message and exit");
}

struct Config {
    bind: SocketAddr,
    token_file: PathBuf,
    android_home: PathBuf,
    avd: String,
}

fn parse_args() -> Result<Config, String> {
    let mut args = std::env::args();
    let program = args.next().unwrap_or_else(|| "fauna-emulator-ctl".into());

    let mut bind_str: Option<String> = None;
    let mut token_file: Option<PathBuf> = None;
    let mut android_home: Option<PathBuf> = None;
    let mut avd: Option<String> = None;

    let remaining = args.collect::<Vec<_>>();
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--help" | "-h" => {
                print_usage(&program);
                std::process::exit(0);
            }
            "--bind" => {
                i += 1;
                bind_str = Some(remaining.get(i).ok_or("--bind requires a value")?.clone());
            }
            "--token-file" => {
                i += 1;
                token_file = Some(PathBuf::from(
                    remaining.get(i).ok_or("--token-file requires a value")?,
                ));
            }
            "--android-home" => {
                i += 1;
                android_home = Some(PathBuf::from(
                    remaining.get(i).ok_or("--android-home requires a value")?,
                ));
            }
            "--avd" => {
                i += 1;
                avd = Some(remaining.get(i).ok_or("--avd requires a value")?.clone());
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
        i += 1;
    }
    drop(remaining);

    // Parse --bind
    let bind_raw = bind_str.as_deref().unwrap_or(DEFAULT_BIND);

    // Reject 0.0.0.0 before parsing so the error is clear
    if bind_raw.starts_with("0.0.0.0") {
        return Err("refusing to bind to 0.0.0.0 — use a specific interface address".into());
    }

    let bind: SocketAddr = bind_raw
        .parse()
        .map_err(|e| format!("invalid --bind address '{bind_raw}': {e}"))?;

    Ok(Config {
        bind,
        token_file: token_file.unwrap_or_else(default_token_file),
        android_home: android_home.unwrap_or_else(default_android_home),
        avd: avd.unwrap_or_else(|| DEFAULT_AVD.into()),
    })
}

// ---------------------------------------------------------------------------
// Token helpers
// ---------------------------------------------------------------------------

fn load_token(path: &PathBuf) -> Result<String, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read token file '{}': {e}", path.display()))?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        return Err(format!("token file '{}' is empty", path.display()));
    }
    Ok(token)
}

/// Constant-time comparison to prevent timing attacks.
fn token_matches(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    query: String,
    auth_token: Option<String>,
}

fn parse_request(stream: &mut TcpStream) -> Result<Request, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set_read_timeout: {e}"))?;

    let mut buf = vec![0u8; MAX_REQUEST_BYTES];
    let n = stream.read(&mut buf).map_err(|e| format!("read: {e}"))?;
    buf.truncate(n);

    let raw = String::from_utf8_lossy(&buf);
    let mut reader = BufReader::new(raw.as_bytes());

    // --- request line ---
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| format!("read_line: {e}"))?;
    let request_line = request_line.trim_end_matches(['\r', '\n']);
    let mut parts = request_line.splitn(3, ' ');
    let method = parts
        .next()
        .ok_or("missing method in request line")?
        .to_string();
    let raw_path = parts
        .next()
        .ok_or("missing path in request line")?
        .to_string();
    // split path from query string
    let (path, query) = if let Some(pos) = raw_path.find('?') {
        (raw_path[..pos].to_string(), raw_path[pos + 1..].to_string())
    } else {
        (raw_path, String::new())
    };

    // --- headers ---
    let mut auth_token: Option<String> = None;
    for line in reader.lines() {
        let line = line.map_err(|e| format!("header read: {e}"))?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break; // end of headers
        }
        // case-insensitive match on "Authorization"
        if line.len() > 14 && line[..14].eq_ignore_ascii_case("authorization:") {
            let value = line[14..].trim();
            if let Some(tok) = value.strip_prefix("Bearer ") {
                auth_token = Some(tok.trim().to_string());
            }
        }
    }

    Ok(Request {
        method,
        path,
        query,
        auth_token,
    })
}

fn send_json(stream: &mut TcpStream, status: u16, status_text: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct AppState {
    token: String,
    android_home: PathBuf,
    avd: String,
    emulator: Mutex<Option<EmulatorState>>,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
}

struct EmulatorState {
    child: Child,
    pid: u32,
    started_at: Instant,
}

#[derive(Clone)]
struct LogLine {
    stream: &'static str, // "stdout" or "stderr"
    text: String,
}

// ---------------------------------------------------------------------------
// Route handlers (stubs)
// ---------------------------------------------------------------------------

fn handle_start(stream: &mut TcpStream, state: &AppState, _req: &Request) {
    let mut emulator_guard = state.emulator.lock().unwrap();

    // Check if an emulator is already running.
    if let Some(ref mut existing) = *emulator_guard {
        match existing.child.try_wait() {
            Ok(None) => {
                // Still running — return 409.
                let pid = existing.pid;
                let body = format!(r#"{{"status":"already_running","pid":{pid},"adb_port":5555}}"#);
                send_json(stream, 409, "Conflict", &body);
                return;
            }
            _ => {
                // Process exited — clear state and proceed.
                *emulator_guard = None;
            }
        }
    }

    // Check that the emulator binary exists.
    let mut emulator_path = state.android_home.clone();
    emulator_path.push("emulator");
    emulator_path.push("emulator");
    if !emulator_path.exists() {
        let msg = format!(
            r#"{{"error":"emulator binary not found","path":"{}"}}"#,
            emulator_path.display()
        );
        send_json(stream, 500, "Internal Server Error", &msg);
        return;
    }

    // Spawn the emulator process.
    let avd = state.avd.clone();
    let mut cmd = Command::new(&emulator_path);
    cmd.args([
        "-avd",
        &avd,
        "-no-window",
        "-gpu",
        "swiftshader_indirect",
        "-no-audio",
        "-no-boot-anim",
        "-no-metrics",
        "-no-snapshot-save",
    ]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!(r#"{{"error":"failed to spawn emulator","detail":"{e}"}}"#);
            send_json(stream, 500, "Internal Server Error", &msg);
            return;
        }
    };

    let pid = child.id();

    // Clear the log buffer and spawn reader threads.
    {
        let mut logs = state.logs.lock().unwrap();
        logs.clear();
    }

    // Take stdout/stderr before moving child into EmulatorState.
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // Spawn stdout reader thread.
    let logs_stdout = Arc::clone(&state.logs);
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let text = match line {
                Ok(t) => t,
                Err(_) => break,
            };
            let mut logs = logs_stdout.lock().unwrap();
            if logs.len() >= MAX_LOG_LINES {
                logs.pop_front();
            }
            logs.push_back(LogLine {
                stream: "stdout",
                text,
            });
        }
    });

    // Spawn stderr reader thread.
    let logs_stderr = Arc::clone(&state.logs);
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let text = match line {
                Ok(t) => t,
                Err(_) => break,
            };
            let mut logs = logs_stderr.lock().unwrap();
            if logs.len() >= MAX_LOG_LINES {
                logs.pop_front();
            }
            logs.push_back(LogLine {
                stream: "stderr",
                text,
            });
        }
    });

    // Wait 2 seconds and check if the process exited immediately.
    std::thread::sleep(Duration::from_secs(2));
    match child.try_wait() {
        Ok(Some(status)) => {
            let msg =
                format!(r#"{{"error":"emulator exited immediately","exit_status":"{status}"}}"#);
            send_json(stream, 500, "Internal Server Error", &msg);
            return;
        }
        Ok(None) => {} // still running — good
        Err(e) => {
            let msg = format!(r#"{{"error":"try_wait failed","detail":"{e}"}}"#);
            send_json(stream, 500, "Internal Server Error", &msg);
            return;
        }
    }

    // Run `adb tcpip 5555` if adb exists.
    let mut adb_path = state.android_home.clone();
    adb_path.push("platform-tools");
    adb_path.push("adb");
    if adb_path.exists() {
        let _ = Command::new(&adb_path).args(["tcpip", "5555"]).status();
    }

    // Store the emulator state.
    *emulator_guard = Some(EmulatorState {
        child,
        pid,
        started_at: Instant::now(),
    });

    let body = format!(r#"{{"status":"started","pid":{pid},"adb_port":5555}}"#);
    send_json(stream, 200, "OK", &body);
}

fn shutdown_emulator(mut es: EmulatorState, state: &AppState) {
    // Try graceful shutdown via `adb emu kill`.
    let mut adb_path = state.android_home.clone();
    adb_path.push("platform-tools");
    adb_path.push("adb");
    if adb_path.exists() {
        let _ = Command::new(&adb_path).args(["emu", "kill"]).status();
    }

    // Wait up to 10 seconds for the process to exit.
    for _ in 0..20 {
        match es.child.try_wait() {
            Ok(Some(_)) => return, // exited cleanly
            Ok(None) => std::thread::sleep(Duration::from_millis(500)),
            Err(e) => {
                eprintln!("warning: try_wait error during graceful shutdown: {e}");
                break;
            }
        }
    }

    // Process still alive — force kill.
    eprintln!("warning: emulator did not exit gracefully; sending SIGKILL");
    let _ = es.child.kill();

    // Wait up to 5 more seconds.
    for _ in 0..10 {
        match es.child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(500)),
            Err(e) => {
                eprintln!("warning: try_wait error after SIGKILL: {e}");
                return;
            }
        }
    }

    eprintln!("warning: emulator process still alive after SIGKILL");
}

fn handle_stop(stream: &mut TcpStream, state: &AppState, _req: &Request) {
    let es = state.emulator.lock().unwrap().take();
    match es {
        None => {
            send_json(stream, 404, "Not Found", r#"{"status":"not_running"}"#);
        }
        Some(es) => {
            shutdown_emulator(es, state);
            send_json(stream, 200, "OK", r#"{"status":"stopped"}"#);
        }
    }
}

fn check_boot_complete(state: &AppState) -> bool {
    let mut adb_path = state.android_home.clone();
    adb_path.push("platform-tools");
    adb_path.push("adb");
    let output = match Command::new(&adb_path)
        .args(["shell", "getprop", "sys.boot_completed"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim() == "1"
}

fn handle_status(stream: &mut TcpStream, state: &AppState, _req: &Request) {
    // Determine what to do under the lock, then act after releasing it.
    enum StatusAction {
        StoppedNone,
        StoppedExited { code: String },
        RunningInfo { pid: u32, uptime: u64 },
        Error(String),
    }

    let action = {
        let mut emulator_guard = state.emulator.lock().unwrap();
        match *emulator_guard {
            Some(ref mut es) => {
                match es.child.try_wait() {
                    Ok(Some(exit_status)) => {
                        let code = exit_status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "null".into());
                        // Clear emulator state
                        *emulator_guard = None;
                        StatusAction::StoppedExited { code }
                    }
                    Ok(None) => {
                        let uptime = es.started_at.elapsed().as_secs();
                        let pid = es.pid;
                        StatusAction::RunningInfo { pid, uptime }
                    }
                    Err(e) => StatusAction::Error(format!("{e}")),
                }
            }
            None => StatusAction::StoppedNone,
        }
    };

    match action {
        StatusAction::StoppedNone => {
            send_json(
                stream,
                200,
                "OK",
                r#"{"status":"stopped","last_exit_code":null}"#,
            );
        }
        StatusAction::StoppedExited { code } => {
            let body = format!(r#"{{"status":"stopped","last_exit_code":{code}}}"#);
            send_json(stream, 200, "OK", &body);
        }
        StatusAction::RunningInfo { pid, uptime } => {
            let boot_complete = check_boot_complete(state);
            let body = format!(
                r#"{{"status":"running","pid":{pid},"adb_port":5555,"boot_complete":{boot_complete},"uptime_seconds":{uptime}}}"#
            );
            send_json(stream, 200, "OK", &body);
        }
        StatusAction::Error(e) => {
            let msg = format!(r#"{{"error":"try_wait failed","detail":"{e}"}}"#);
            send_json(stream, 500, "Internal Server Error", &msg);
        }
    }
}

fn handle_logs(stream: &mut TcpStream, state: &AppState, req: &Request) {
    // Parse ?tail=N from query string; default 100, max MAX_LOG_LINES
    let tail: usize = req
        .query
        .split('&')
        .find_map(|kv| {
            let (key, val) = kv.split_once('=')?;

            if key == "tail" {
                val.parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(100)
        .min(MAX_LOG_LINES);

    let logs = state.logs.lock().unwrap();
    let total = logs.len();
    let skip = total.saturating_sub(tail);

    let mut json_lines = String::new();
    let mut first = true;
    for line in logs.iter().skip(skip) {
        if !first {
            json_lines.push(',');
        }
        first = false;
        // Escape JSON special characters in text
        let mut escaped = String::with_capacity(line.text.len());
        for ch in line.text.chars() {
            match ch {
                '\\' => escaped.push_str("\\\\"),
                '"' => escaped.push_str("\\\""),
                '\n' => escaped.push_str("\\n"),
                '\r' => escaped.push_str("\\r"),
                '\t' => escaped.push_str("\\t"),
                c => escaped.push(c),
            }
        }
        json_lines.push_str(&format!(
            r#"{{"stream":"{}","text":"{}"}}"#,
            line.stream, escaped
        ));
    }

    let body = format!(r#"{{"lines":[{json_lines}],"total_captured":{total}}}"#);
    send_json(stream, 200, "OK", &body);
}

// ---------------------------------------------------------------------------
// Connection handler
// ---------------------------------------------------------------------------

fn handle_connection(mut stream: TcpStream, state: &AppState) {
    let req = match parse_request(&mut stream) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("parse_request error: {e}");
            send_json(
                &mut stream,
                400,
                "Bad Request",
                r#"{"error":"bad request"}"#,
            );
            return;
        }
    };

    // Auth check
    let provided = req.auth_token.as_deref().unwrap_or("");
    if !token_matches(provided, &state.token) {
        send_json(
            &mut stream,
            401,
            "Unauthorized",
            r#"{"error":"unauthorized"}"#,
        );
        return;
    }

    // Route
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/start") => handle_start(&mut stream, state, &req),
        ("POST", "/stop") => handle_stop(&mut stream, state, &req),
        ("GET", "/status") => handle_status(&mut stream, state, &req),
        ("GET", "/logs") => handle_logs(&mut stream, state, &req),
        _ => send_json(&mut stream, 404, "Not Found", r#"{"error":"not found"}"#),
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let cfg = parse_args().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });

    let token = load_token(&cfg.token_file).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });

    let state = Arc::new(AppState {
        token,
        android_home: cfg.android_home,
        avd: cfg.avd,
        emulator: Mutex::new(None),
        logs: Arc::new(Mutex::new(VecDeque::new())),
    });

    let listener = TcpListener::bind(cfg.bind).unwrap_or_else(|e| {
        eprintln!("error: cannot bind to {}: {e}", cfg.bind);
        std::process::exit(1);
    });

    // Register signal handlers for clean shutdown.
    unsafe {
        signal(2, signal_handler); // SIGINT
        signal(15, signal_handler); // SIGTERM
    }

    eprintln!("listening on {}", cfg.bind);

    listener.set_nonblocking(true).ok();
    loop {
        if SHUTDOWN_REQUESTED.load(Ordering::Relaxed) {
            eprintln!("\nshutting down...");
            let mut emu = state.emulator.lock().unwrap();
            if let Some(es) = emu.take() {
                shutdown_emulator(es, &state);
            }
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || handle_connection(stream, &state));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}
