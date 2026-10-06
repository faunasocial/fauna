// unix-only end to end: the mock SDK binaries are `#!/bin/bash` scripts made
// executable via unix mode bits, and the emulator itself is only ever driven
// from a unix host (the Mac host or its Linux dev VM — the android emulator
// never runs on Windows).
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn write_token_file(dir: &tempfile::TempDir) -> PathBuf {
    let path = dir.path().join("token");
    std::fs::write(&path, "test-token-abc123\n").expect("write token file");
    path
}

/// Finds a free port by briefly binding 0.0.0.0:0 and recording the assigned port.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind port 0")
        .local_addr()
        .expect("local_addr")
        .port()
}

/// Creates a fake ANDROID_HOME with mock emulator and adb binaries.
fn create_mock_sdk(dir: &std::path::Path) -> PathBuf {
    let sdk = dir.join("android-sdk");
    let emulator_dir = sdk.join("emulator");
    let platform_tools = sdk.join("platform-tools");
    std::fs::create_dir_all(&emulator_dir).unwrap();
    std::fs::create_dir_all(&platform_tools).unwrap();

    // Mock emulator — writes to stderr then sleeps forever
    let emulator_bin = emulator_dir.join("emulator");
    std::fs::write(
        &emulator_bin,
        "#!/bin/bash\necho \"emulator: INFO: boot completed\" >&2\necho \"emulator: INFO: listening\" >&2\nwhile true; do sleep 1; done\n",
    ).unwrap();
    std::fs::set_permissions(&emulator_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Mock adb — just exits successfully
    let adb_bin = platform_tools.join("adb");
    std::fs::write(&adb_bin, "#!/bin/bash\nexit 0\n").unwrap();
    std::fs::set_permissions(&adb_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    sdk
}

fn start_server(token_file: &std::path::Path, port: u16) -> Child {
    // Locate the binary. Cargo sets CARGO_BIN_EXE_fauna-emulator-ctl at
    // compile time for integration tests.
    let bin = env!("CARGO_BIN_EXE_fauna-emulator-ctl");
    let child = Command::new(bin)
        .args([
            "--bind",
            &format!("127.0.0.1:{port}"),
            "--token-file",
            token_file.to_str().expect("token path utf8"),
            "--android-home",
            "/nonexistent",
            "--avd",
            "test-avd",
        ])
        .spawn()
        .expect("spawn server");
    // Give the server a moment to bind the port.
    std::thread::sleep(Duration::from_millis(200));
    child
}

fn start_server_with_sdk(
    token_file: &std::path::Path,
    port: u16,
    sdk_path: &std::path::Path,
) -> Child {
    let bin = env!("CARGO_BIN_EXE_fauna-emulator-ctl");
    let child = Command::new(bin)
        .args([
            "--bind",
            &format!("127.0.0.1:{port}"),
            "--token-file",
            token_file.to_str().unwrap(),
            "--android-home",
            sdk_path.to_str().unwrap(),
            "--avd",
            "test-avd",
        ])
        .spawn()
        .expect("spawn server");
    std::thread::sleep(Duration::from_millis(200));
    child
}

/// Sends a raw HTTP/1.1 request and returns (status_code, body).
/// Retries connection up to 10 times with 100ms delay to handle server startup race.
fn http_request(port: u16, method: &str, path: &str, token: Option<&str>) -> (u16, String) {
    http_request_timeout(port, method, path, token, Duration::from_secs(10))
}

fn http_request_timeout(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    timeout: Duration,
) -> (u16, String) {
    let addr = format!("127.0.0.1:{port}");
    let mut stream = None;
    for _ in 0..10 {
        match TcpStream::connect(&addr) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let mut stream = stream.unwrap_or_else(|| panic!("could not connect to {addr} after retries"));
    stream.set_read_timeout(Some(timeout)).unwrap();

    let auth_header = match token {
        Some(t) => format!("Authorization: Bearer {t}\r\n"),
        None => String::new(),
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth_header}Connection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).expect("write request");

    // Read response headers (until \r\n\r\n), then read body by Content-Length.
    // This avoids relying on EOF which can cause read_to_string to block until
    // the server fully closes the TCP connection.
    let mut header_buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).expect("read header byte");
        header_buf.push(byte[0]);
        if header_buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let header_str = String::from_utf8_lossy(&header_buf);

    // Parse status line: "HTTP/1.1 NNN <text>"
    let status_code = header_str
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .expect("parse status code");

    // Parse Content-Length header
    let content_length: usize = header_str
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("content-length:") {
                line.split_once(':')?.1.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    // Read exactly content_length bytes for the body
    let mut body_buf = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body_buf).expect("read body");
    }
    let body = String::from_utf8_lossy(&body_buf).into_owned();

    (status_code, body)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_missing_token_returns_401() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = write_token_file(&dir);
    let port = free_port();
    let mut child = start_server(&token_file, port);

    let (status, body) = http_request(port, "GET", "/status", None);
    child.kill().ok();
    child.wait().ok();

    assert_eq!(status, 401, "expected 401, got {status}: {body}");
    assert!(body.contains("unauthorized"), "body: {body}");
}

#[test]
fn test_wrong_token_returns_401() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = write_token_file(&dir);
    let port = free_port();
    let mut child = start_server(&token_file, port);

    let (status, body) = http_request(port, "GET", "/status", Some("wrong-token"));
    child.kill().ok();
    child.wait().ok();

    assert_eq!(status, 401, "expected 401, got {status}: {body}");
    assert!(body.contains("unauthorized"), "body: {body}");
}

#[test]
fn test_valid_token_routes_correctly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = write_token_file(&dir);
    let port = free_port();
    let mut child = start_server(&token_file, port);

    let (status, body) = http_request(port, "GET", "/status", Some("test-token-abc123"));
    child.kill().ok();
    child.wait().ok();

    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    assert!(body.contains("stopped"), "body: {body}");
}

#[test]
fn test_unknown_route_returns_404() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = write_token_file(&dir);
    let port = free_port();
    let mut child = start_server(&token_file, port);

    let (status, body) = http_request(port, "GET", "/unknown", Some("test-token-abc123"));
    child.kill().ok();
    child.wait().ok();

    assert_eq!(status, 404, "expected 404, got {status}: {body}");
    assert!(body.contains("not found"), "body: {body}");
}

#[test]
fn test_status_returns_stopped_when_no_emulator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = write_token_file(&dir);
    let port = free_port();
    let mut child = start_server(&token_file, port);

    let (status, body) = http_request(port, "GET", "/status", Some("test-token-abc123"));
    child.kill().ok();
    child.wait().ok();

    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    assert!(
        body.contains("stopped"),
        "body should contain 'stopped': {body}"
    );
    assert!(
        body.contains("last_exit_code"),
        "body should contain 'last_exit_code': {body}"
    );
}

#[test]
fn test_logs_returns_empty_when_no_emulator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = write_token_file(&dir);
    let port = free_port();
    let mut child = start_server(&token_file, port);

    let (status, body) = http_request(port, "GET", "/logs?tail=10", Some("test-token-abc123"));
    child.kill().ok();
    child.wait().ok();

    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    assert!(
        body.contains(r#""lines":[]"#),
        "body should contain empty lines array: {body}"
    );
    assert!(
        body.contains(r#""total_captured":0"#),
        "body should contain total_captured:0: {body}"
    );
}

#[test]
fn test_full_lifecycle_start_status_logs_stop() {
    let dir = tempfile::tempdir().unwrap();
    let token_file = write_token_file(&dir);
    let sdk = create_mock_sdk(dir.path());
    let port = free_port();
    let mut server = start_server_with_sdk(&token_file, port, &sdk);
    let token = "test-token-abc123";

    // 1. Start — should succeed (mock emulator script runs).
    // Use a 15s timeout because the /start handler sleeps 2s to check for crash.
    let (status, body) =
        http_request_timeout(port, "POST", "/start", Some(token), Duration::from_secs(15));
    assert_eq!(status, 200, "start: {body}");
    assert!(body.contains(r#""status":"started""#), "start body: {body}");
    assert!(body.contains(r#""adb_port":5555"#), "start body: {body}");

    // 2. Start again — should get 409 (already running).
    // Also use a long timeout in case the server is briefly busy.
    let (status, body) =
        http_request_timeout(port, "POST", "/start", Some(token), Duration::from_secs(15));
    assert_eq!(status, 409, "double start: {body}");
    assert!(
        body.contains(r#""status":"already_running""#),
        "double start body: {body}"
    );

    // 3. Give log capture threads time to read stderr from mock emulator
    std::thread::sleep(Duration::from_secs(1));

    // 4. Status — should be running
    let (status, body) = http_request(port, "GET", "/status", Some(token));
    assert_eq!(status, 200, "status: {body}");
    assert!(
        body.contains(r#""status":"running""#),
        "status body: {body}"
    );

    // 5. Logs — should have captured mock emulator stderr output
    let (status, body) = http_request(port, "GET", "/logs?tail=10", Some(token));
    assert_eq!(status, 200, "logs: {body}");
    assert!(body.contains("boot completed"), "logs body: {body}");
    assert!(body.contains(r#""stream":"stderr""#), "logs body: {body}");

    // 6. Stop — should succeed.
    // shutdown_emulator waits up to 10s for graceful exit + 5s after SIGKILL = 15s max.
    let (status, body) =
        http_request_timeout(port, "POST", "/stop", Some(token), Duration::from_secs(20));
    assert_eq!(status, 200, "stop: {body}");
    assert!(body.contains(r#""status":"stopped""#), "stop body: {body}");

    // 7. Stop again — should get 404
    let (status, body) = http_request(port, "POST", "/stop", Some(token));
    assert_eq!(status, 404, "double stop: {body}");
    assert!(
        body.contains(r#""status":"not_running""#),
        "double stop body: {body}"
    );

    // 8. Status after stop — should be stopped
    let (status, body) = http_request(port, "GET", "/status", Some(token));
    assert_eq!(status, 200, "final status: {body}");
    assert!(
        body.contains(r#""status":"stopped""#),
        "final status body: {body}"
    );

    server.kill().ok();
    server.wait().ok();
}

// ---------------------------------------------------------------------------
// Smoke tests (CLI behavior)
// ---------------------------------------------------------------------------

#[test]
fn test_help_flag_exits_zero() {
    let bin = env!("CARGO_BIN_EXE_fauna-emulator-ctl");
    let output = Command::new(bin)
        .arg("--help")
        .output()
        .expect("run --help");
    assert!(output.status.success(), "exit code: {:?}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--bind"), "stderr: {stderr}");
    assert!(stderr.contains("--token-file"), "stderr: {stderr}");
    assert!(stderr.contains("--android-home"), "stderr: {stderr}");
    assert!(stderr.contains("--avd"), "stderr: {stderr}");
}

#[test]
fn test_bind_0000_rejected() {
    let bin = env!("CARGO_BIN_EXE_fauna-emulator-ctl");
    let output = Command::new(bin)
        .args(["--bind", "0.0.0.0:7370"])
        .output()
        .expect("run with 0.0.0.0");
    assert!(!output.status.success(), "should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refusing to bind to 0.0.0.0"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_missing_token_file_exits_with_error() {
    let bin = env!("CARGO_BIN_EXE_fauna-emulator-ctl");
    let output = Command::new(bin)
        .args(["--token-file", "/nonexistent/token"])
        .output()
        .expect("run with missing token file");
    assert!(!output.status.success(), "should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot read token file"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_empty_token_file_exits_with_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_path = dir.path().join("empty-token");
    std::fs::write(&token_path, "").unwrap();

    let bin = env!("CARGO_BIN_EXE_fauna-emulator-ctl");
    let output = Command::new(bin)
        .args(["--token-file", token_path.to_str().unwrap()])
        .output()
        .expect("run with empty token file");
    assert!(!output.status.success(), "should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("empty"), "stderr: {stderr}");
}

#[test]
fn test_unknown_flag_exits_with_error() {
    let bin = env!("CARGO_BIN_EXE_fauna-emulator-ctl");
    let output = Command::new(bin)
        .arg("--bogus")
        .output()
        .expect("run with unknown flag");
    assert!(!output.status.success(), "should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown argument"), "stderr: {stderr}");
}

#[test]
fn test_start_with_missing_emulator_binary_returns_500() {
    let dir = tempfile::tempdir().unwrap();
    let token_file = write_token_file(&dir);
    // Use /nonexistent as android-home — no emulator binary there
    let port = free_port();
    let mut server = start_server(&token_file, port);

    let (status, body) = http_request_timeout(
        port,
        "POST",
        "/start",
        Some("test-token-abc123"),
        Duration::from_secs(10),
    );
    server.kill().ok();
    server.wait().ok();

    assert_eq!(status, 500, "start with no emulator: {body}");
    assert!(body.contains("emulator"), "body: {body}");
}
