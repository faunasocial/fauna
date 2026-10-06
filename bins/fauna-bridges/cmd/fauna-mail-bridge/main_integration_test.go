// Integration tests for the fauna-mail-bridge binary's wire-up.
//
// Gated by FAUNA_MAIL_BRIDGE_E2E=1 because they:
//   - go build the binary into a temp dir (slow on cold cache)
//   - bind an ephemeral TCP port for the metrics listener
//   - spawn the binary as a subprocess (process semantics matter:
//     SIGTERM handling, exit code, /metrics actually served on a
//     real port — the in-process `run()` tests can't exercise this)
//
// The test wires a stand-in nest via httptest.NewServer that:
//   - handles HTTP POST /api/v1/auth/{challenge,verify} with a
//     deterministic fake token (no real Ed25519 verify on the mock —
//     the bridge's signing path is exercised by the wsrpc unit tests
//     already; the integration test cares about end-to-end flow);
//   - accepts the WS upgrade at /api/v1/ws/<actor_id_hex>;
//   - responds to fauna.bridges.whoami, fauna.bridges.fetch_config,
//     and fauna.bridges.fetch_tls_cert_blob with synthetic replies
//     drawn from the real Rust-side fixtures (testdata/wrapped-cert
//     .cbor for the TLS-cert path).
//
// TestBridgeStartsAndIdles exercises the "mail enabled" path:
// nest returns a non-zero ConfigSnapshot, the bridge fetches the
// TLS cert blob (Whoami.Domain non-empty), spawns /metrics, idles.
// SIGTERM → exit 0.
//
// TestBridgeIdlesWhenMailDisabled exercises the "mail off" path:
// nest returns the all-zero ConfigSnapshot (MailEnabled=false,
// per Phase C.0's explicit field on FetchConfigReply). The bridge
// still enrolls, /metrics still serves, idles cleanly. This is the
// regression guard for the product-invariant "fresh nest has mail
// off by default" — Phase E's nest-side flag-file dance will gate
// process startup at the supervisor level, but the bridge itself
// must not crash when started against a mail-off snapshot (e.g.
// mid-transition).

package main

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"nhooyr.io/websocket"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/keypair"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// registerServiceUserPeek mirrors the wsrpc-internal (unexported)
// registerServiceUserRequest so the mock nest can read the attested
// pubkeys off the wire. Field tags match the Rust
// RegisterServiceUserRequest (serde_bytes → CBOR byte string).
type registerServiceUserPeek struct {
	Ed25519Pubkey []byte `cbor:"ed25519_pubkey"`
	X25519Pubkey  []byte `cbor:"x25519_pubkey"`
	Role          string `cbor:"role"`
	BridgeID      string `cbor:"bridge_id"`
}

// integrationGate skips the test unless FAUNA_MAIL_BRIDGE_E2E=1.
// The integration tests build the binary and spawn it as a
// subprocess; that's heavier than the unit tests, so the gate
// keeps `go test ./...` fast on the default path.
func integrationGate(t *testing.T) {
	t.Helper()
	if os.Getenv("FAUNA_MAIL_BRIDGE_E2E") != "1" {
		t.Skip("set FAUNA_MAIL_BRIDGE_E2E=1 to run the bridge integration tests")
	}
}

// fixtureWrappedCert reads the wrapped-cert.cbor fixture that the
// internal/tls package uses for its unit tests. The fixture is
// sealed against X25519 secret=[1u8; 32]; we generate the test
// keyfile with the same secret so the bridge's HPKE-Open succeeds.
//
// Returns the raw wrapped-blob bytes ready to ride as the
// fauna.bridges.fetch_tls_cert_blob reply payload.
func fixtureWrappedCert(t *testing.T) []byte {
	t.Helper()
	// The fixture lives next to internal/tls/tls_test.go. Walk up
	// from the cmd/fauna-mail-bridge directory to find it.
	candidates := []string{
		"../../internal/tls/testdata/wrapped-cert.cbor",
		"internal/tls/testdata/wrapped-cert.cbor",
	}
	for _, p := range candidates {
		b, err := os.ReadFile(p)
		if err == nil {
			return b
		}
	}
	t.Fatalf("could not find internal/tls/testdata/wrapped-cert.cbor in any of %v", candidates)
	return nil
}

// fixtureX25519Secret is the secret used to seal the wrapped-cert
// fixture above. Must match the Rust fixture-writer's recipient
// secret (see tls_test.go header comment).
var fixtureX25519Secret = func() []byte {
	s := make([]byte, 32)
	for i := range s {
		s[i] = 0x01
	}
	return s
}()

// writeTestKeyfile generates a Phase B.8-shaped keyfile (placeholder
// role="unresolved") with the deterministic X25519 secret the TLS
// fixture is sealed against. Returns the path.
func writeTestKeyfile(t *testing.T, dir string) string {
	t.Helper()
	path := filepath.Join(dir, "keyfile.cbor")
	// Generate to get a fresh Ed25519 seed (the integration test
	// doesn't care about Ed25519 identity — the mock nest issues
	// tokens unconditionally; only X25519 matters for the TLS
	// path).
	kf, err := keypair.Generate("unresolved", "unresolved-bridge", uint64(time.Now().Unix()))
	if err != nil {
		t.Fatalf("keypair.Generate: %v", err)
	}
	// Overwrite X25519 with the fixture secret so the bridge can
	// HPKE-Open the wrapped-cert blob.
	copy(kf.X25519Priv, fixtureX25519Secret)
	raw, err := kf.Marshal()
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}
	if err := os.WriteFile(path, raw, 0o600); err != nil {
		t.Fatalf("write keyfile: %v", err)
	}
	return path
}

// expectedX25519Pubkey loads the keyfile at path and returns its
// derived X25519 public key — the value the bridge must attest to nest
// via register_service_user (Stage-5 Gap E).
func expectedX25519Pubkey(t *testing.T, keypairPath string) []byte {
	t.Helper()
	raw, err := os.ReadFile(keypairPath)
	if err != nil {
		t.Fatalf("read keyfile: %v", err)
	}
	kf, err := keypair.Unmarshal(raw)
	if err != nil {
		t.Fatalf("unmarshal keyfile: %v", err)
	}
	pub := kf.X25519PublicKey()
	return pub[:]
}

// freeTCPAddr binds a TCP socket on 127.0.0.1:0, captures the kernel-
// assigned port, and closes the socket. Returns "127.0.0.1:<port>".
// Race-safe: the kernel reserves the port briefly enough that a
// follow-up bind from the bridge process will win in practice; if
// it does race, the test will fail loudly on the bridge's
// listener-bind error, which is what we want.
func freeTCPAddr(t *testing.T) string {
	t.Helper()
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen 127.0.0.1:0: %v", err)
	}
	addr := l.Addr().String()
	_ = l.Close()
	return addr
}

// mockNest holds the synthetic nest's state. It mirrors the
// internal/wsrpc tests' mock pattern (HTTP /auth/{challenge,verify}
// + WS at /api/v1/ws/<actor>) but adds two RPC method handlers:
// fauna.bridges.whoami and fauna.bridges.fetch_config. The
// fauna.bridges.fetch_tls_cert_blob handler is wired only when
// mailEnabled is true (the domain is non-empty in WhoamiReply).
type mockNest struct {
	srv     *httptest.Server
	whoami  wsrpc.WhoamiReply
	config  wsrpc.ConfigSnapshot
	tlsBlob []byte
	wg      sync.WaitGroup

	// callCounts records how many times each method was invoked.
	mu         sync.Mutex
	callCounts map[string]int

	// attested{Ed25519,X25519} capture the pubkeys the bridge sends in
	// fauna.bridges.register_service_user (Stage-5 Gap E). Nil until the
	// call arrives. Read under mu via attested().
	attestedEd25519 []byte
	attestedX25519  []byte
}

// attested returns the pubkeys captured from the bridge's
// register_service_user call (nil if it never arrived).
func (m *mockNest) attested() (ed, x []byte) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.attestedEd25519, m.attestedX25519
}

func (m *mockNest) recordCall(method string) {
	m.mu.Lock()
	m.callCounts[method]++
	m.mu.Unlock()
}

func (m *mockNest) calls(method string) int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.callCounts[method]
}

// startMockNest wires the HTTP + WS handlers and returns a running
// httptest server. Caller cleans up via t.Cleanup.
func startMockNest(t *testing.T, whoami wsrpc.WhoamiReply, snapshot wsrpc.ConfigSnapshot, tlsBlob []byte) *mockNest {
	t.Helper()
	m := &mockNest{
		whoami:     whoami,
		config:     snapshot,
		tlsBlob:    tlsBlob,
		callCounts: make(map[string]int),
	}
	mux := http.NewServeMux()
	mux.HandleFunc("/api/v1/auth/challenge", func(w http.ResponseWriter, r *http.Request) {
		// Mirror the real nest's shape (32-byte hex nonce).
		_ = json.NewEncoder(w).Encode(map[string]any{
			"nonce":      hex.EncodeToString(make([]byte, 32)),
			"expires_in": 300,
			"expires_at": time.Now().Unix() + 300,
		})
	})
	mux.HandleFunc("/api/v1/auth/verify", func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"token":      "integration-mock-token",
			"expires_in": 3600,
			"expires_at": time.Now().Unix() + 3600,
		})
	})
	mux.HandleFunc("/api/v1/ws/", func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
			Subprotocols: []string{"fauna.v1"},
		})
		if err != nil {
			t.Logf("ws accept: %v", err)
			return
		}
		defer conn.Close(websocket.StatusNormalClosure, "")
		conn.SetReadLimit(16 << 20)
		m.wg.Add(1)
		defer m.wg.Done()
		m.serveWS(t, conn)
	})
	m.srv = httptest.NewServer(mux)
	t.Cleanup(func() {
		m.srv.Close()
		m.wg.Wait()
	})
	return m
}

func (m *mockNest) serveWS(t *testing.T, conn *websocket.Conn) {
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		_, data, err := conn.Read(ctx)
		cancel()
		if err != nil {
			return
		}
		frame, err := wsrpc.DecodeFrame(data)
		if err != nil {
			t.Logf("decode frame: %v", err)
			continue
		}
		req, ok := frame.(*wsrpc.RequestFrame)
		if !ok {
			continue
		}
		m.recordCall(req.Kind)
		var (
			replyBody any
			replyOK   = true
		)
		switch req.Kind {
		case wsrpc.MethodWhoami:
			replyBody = m.whoami
		case wsrpc.MethodRegisterServiceUser:
			// Stage-5 Gap E: the bridge attests its x25519 pubkey so
			// nest can seal the TLS cert blob to it. Capture
			// the attested keys so the test can assert they match the
			// bridge's keyfile, then reply with a synthetic
			// enrollment_request_id (RegisterServiceUserReply shape).
			if reg, derr := dagcbor.Unmarshal[registerServiceUserPeek](req.Payload); derr == nil {
				m.mu.Lock()
				m.attestedEd25519 = reg.Ed25519Pubkey
				m.attestedX25519 = reg.X25519Pubkey
				m.mu.Unlock()
			} else {
				t.Logf("mock nest: decode register_service_user payload: %v", derr)
			}
			replyBody = map[string]any{"enrollment_request_id": "integration-enrollment-approved"}
		case wsrpc.MethodFetchConfig:
			replyBody = m.config
		case wsrpc.MethodFetchTLSCertBlob:
			// The reply field is Option<ByteBuf> on the Rust side;
			// the Go-side wrapper expects {"blob": <bytes> | null}.
			// We construct that map by hand to avoid depending on
			// the unexported reply struct.
			if m.tlsBlob == nil {
				replyBody = map[string]any{"blob": nil}
			} else {
				replyBody = map[string]any{"blob": m.tlsBlob}
			}
		default:
			t.Logf("mock nest: unknown method %q", req.Kind)
			replyOK = false
			replyBody = map[string]any{"error": "unknown method"}
		}
		repBytes, err := wsrpc.EncodeReplyForTest(req.CorrelationID, replyBody, replyOK)
		if err != nil {
			t.Logf("encode reply for %q: %v", req.Kind, err)
			continue
		}
		writeCtx, wcancel := context.WithTimeout(context.Background(), 5*time.Second)
		err = conn.Write(writeCtx, websocket.MessageBinary, repBytes)
		wcancel()
		if err != nil {
			return
		}
	}
}

// buildBridgeBinary compiles the bridge binary into a temp directory
// and returns its path. CGO settings (CGO_CFLAGS / CGO_LDFLAGS /
// LD_LIBRARY_PATH) are inherited from the test environment, which
// `just mail-bridge-test` populates via the same recipe used to run
// the unit tests. If the env is not populated, this test will surface
// a clear go-build error.
func buildBridgeBinary(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	out := filepath.Join(dir, "fauna-mail-bridge")
	if runtime.GOOS == "windows" {
		out += ".exe"
	}
	// Locate the cmd directory by walking up from the test file's
	// own working directory at runtime — go test sets CWD to the
	// package directory.
	cwd, err := os.Getwd()
	if err != nil {
		t.Fatalf("os.Getwd: %v", err)
	}
	cmd := exec.Command("go", "build", "-o", out, ".")
	cmd.Dir = cwd
	var buf bytes.Buffer
	cmd.Stdout = &buf
	cmd.Stderr = &buf
	if err := cmd.Run(); err != nil {
		t.Fatalf("go build: %v\n%s", err, buf.String())
	}
	return out
}

// runBridgeSubprocess starts the bridge binary as a subprocess and
// returns a handle the test uses to wait for output / send signals
// / collect exit status.
type bridgeProc struct {
	cmd    *exec.Cmd
	stdout *bytes.Buffer
	stderr *bytes.Buffer
}

func startBridge(t *testing.T, binPath, keypairPath, nestEndpoint, dataDir string) *bridgeProc {
	t.Helper()
	args := []string{
		"--keypair-file=" + keypairPath,
		"--nest-endpoint=" + nestEndpoint,
		"--log-level=debug",
	}
	if dataDir != "" {
		args = append(args, "--data-dir="+dataDir)
	}
	cmd := exec.Command(binPath, args...)
	stdout := &bytes.Buffer{}
	stderr := &bytes.Buffer{}
	cmd.Stdout = stdout
	cmd.Stderr = stderr
	// Inherit LD_LIBRARY_PATH from the parent so the bridge can
	// dlopen libfauna_ffi.so. The justfile mail-bridge-test recipe
	// sets this; go test inherits it.
	cmd.Env = os.Environ()
	setProcGroup(cmd)
	if err := cmd.Start(); err != nil {
		t.Fatalf("start bridge: %v", err)
	}
	bp := &bridgeProc{cmd: cmd, stdout: stdout, stderr: stderr}
	t.Cleanup(func() {
		if cmd.ProcessState == nil || !cmd.ProcessState.Exited() {
			// Best-effort group kill to avoid leaks if a test bailed.
			killProcGroup(cmd)
			_ = cmd.Wait()
		}
	})
	return bp
}

// waitForReady polls stderr for the "fauna-mail-bridge ready" log
// line within a deadline. The bridge logs JSON to stderr (per
// internal/logging); we substring-match the message.
func (bp *bridgeProc) waitForReady(t *testing.T, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if strings.Contains(bp.stderr.String(), `"fauna-mail-bridge ready"`) {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("bridge not ready within %v\nstderr:\n%s\nstdout:\n%s", timeout, bp.stderr.String(), bp.stdout.String())
}

// sigtermAndWait sends SIGTERM and waits up to timeout for clean
// exit. Returns the exit error (nil = exit 0).
func (bp *bridgeProc) sigtermAndWait(t *testing.T, timeout time.Duration) error {
	t.Helper()
	if err := bp.cmd.Process.Signal(syscall.SIGTERM); err != nil {
		t.Fatalf("signal SIGTERM: %v", err)
	}
	exitCh := make(chan error, 1)
	go func() { exitCh <- bp.cmd.Wait() }()
	select {
	case err := <-exitCh:
		return err
	case <-time.After(timeout):
		t.Fatalf("bridge did not exit within %v after SIGTERM\nstderr:\n%s", timeout, bp.stderr.String())
		return nil
	}
}

// scrapeMetrics fetches /metrics from the bridge's metrics listener.
// We discover the bind address from the operator-hatch fixture the
// test writes; that's a contract the test owns end-to-end.
func scrapeMetrics(t *testing.T, addr string) string {
	t.Helper()
	resp, err := http.Get("http://" + addr + "/metrics")
	if err != nil {
		t.Fatalf("GET /metrics: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("GET /metrics: status %d", resp.StatusCode)
	}
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("read /metrics: %v", err)
	}
	return string(body)
}

// makeOperatorHatch writes a minimal operator-hatch TOML pinning
// metrics_bind_addr to the supplied free port, so the test knows
// where to scrape /metrics. When mtaBindAddr is non-empty, also
// pins mta_bind_addr / mta_bind_addr_465 / mta_bind_addr_587 —
// required for the MTA-role test path so the bridge doesn't try to
// bind privileged :25 / :465 / :587 inside the test sandbox. Each
// of the three is set if its corresponding argument is non-empty.
func makeOperatorHatch(t *testing.T, dataDir, metricsBindAddr, mtaBindAddr, mtaBindAddr465, mtaBindAddr587 string) {
	t.Helper()
	body := "metrics_bind_addr = \"" + metricsBindAddr + "\"\n"
	if mtaBindAddr != "" {
		body += "mta_bind_addr = \"" + mtaBindAddr + "\"\n"
	}
	if mtaBindAddr465 != "" {
		body += "mta_bind_addr_465 = \"" + mtaBindAddr465 + "\"\n"
	}
	if mtaBindAddr587 != "" {
		body += "mta_bind_addr_587 = \"" + mtaBindAddr587 + "\"\n"
	}
	path := filepath.Join(dataDir, "operator-hatch.toml")
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatalf("write operator-hatch: %v", err)
	}
}

// enabledConfigSnapshot returns the catalog config snapshot — the
// production shape internal/mta.MailEnabled reads MailEnabled=true
// off (Phase C.0 removed the Spam-derived heuristic). Catalog values
// flow from wsrpc.DefaultConfigSnapshot, which mirrors the Rust
// FetchConfigReply::default() in libs/fauna-protocol. LocalDomains
// + PrimaryDomain stay at their default empty values; tests that
// want a non-empty list use enabledConfigSnapshotForDomain.
func enabledConfigSnapshot() wsrpc.ConfigSnapshot {
	return wsrpc.DefaultConfigSnapshot()
}

// enabledConfigSnapshotForDomain returns the catalog snapshot with
// MailEnabled=true and a single-domain LocalDomains projection. Used
// by integration tests where the bridge must accept RCPT TO for that
// domain (the TLS-cert fixture binds to it). The same string serves
// as the PrimaryDomain anchor for TLS/EHLO.
func enabledConfigSnapshotForDomain(domain string) wsrpc.ConfigSnapshot {
	snap := wsrpc.DefaultConfigSnapshot()
	snap.LocalDomains = []string{domain}
	snap.PrimaryDomain = domain
	return snap
}

// disabledConfigSnapshot is the all-zero ConfigSnapshot —
// MailEnabled=false drives internal/mta.Run down the idle path. The
// bridge must still start, enroll, expose /metrics, and idle.
func disabledConfigSnapshot() wsrpc.ConfigSnapshot {
	return wsrpc.ConfigSnapshot{}
}

// TestBridgeStartsAndIdles — happy path. Bridge enrolls, FetchConfig
// gets the "mail enabled" snapshot, TLS cert is fetched + unwrapped
// (Whoami.Domain is non-empty), /metrics serves on the operator-
// hatch-supplied port. SIGTERM → exit 0 within 5s.
func TestBridgeStartsAndIdles(t *testing.T) {
	integrationGate(t)

	tmpDir := t.TempDir()
	keypairPath := writeTestKeyfile(t, tmpDir)
	metricsAddr := freeTCPAddr(t)
	// MTA listeners use ephemeral non-privileged ports — the test
	// sandbox doesn't have CAP_NET_BIND_SERVICE for :25 / :465 / :587.
	mtaAddr := freeTCPAddr(t)
	mtaAddr465 := freeTCPAddr(t)
	mtaAddr587 := freeTCPAddr(t)
	makeOperatorHatch(t, tmpDir, metricsAddr, mtaAddr, mtaAddr465, mtaAddr587)

	whoami := wsrpc.WhoamiReply{
		Role:             "mta",
		BridgeID:         "integration-mta-1",
		Status:           "approved",
		Ed25519PubkeyHex: strings.Repeat("ee", 32),
		X25519PubkeyHex:  strings.Repeat("ff", 32),
	}
	tlsBlob := fixtureWrappedCert(t)
	// snapshot.PrimaryDomain = "test.example.com" matches the
	// TLS-cert fixture (multi-domain refactor moved per-bridge
	// domain from WhoamiReply to ConfigSnapshot).
	m := startMockNest(t, whoami, enabledConfigSnapshotForDomain("test.example.com"), tlsBlob)

	bin := buildBridgeBinary(t)
	bp := startBridge(t, bin, keypairPath, m.srv.URL, tmpDir)
	bp.waitForReady(t, 10*time.Second)

	// Verify the three Phase B.8 bootstrap RPCs all fired.
	if c := m.calls(wsrpc.MethodWhoami); c < 1 {
		t.Fatalf("whoami calls = %d, want >= 1", c)
	}
	if c := m.calls(wsrpc.MethodFetchConfig); c < 1 {
		t.Fatalf("fetch_config calls = %d, want >= 1", c)
	}
	if c := m.calls(wsrpc.MethodFetchTLSCertBlob); c < 1 {
		t.Fatalf("fetch_tls_cert_blob calls = %d, want >= 1", c)
	}

	// Stage-5 Gap E: the bridge attests its x25519 pubkey so nest can
	// seal the TLS cert blob to it (storage-modes.md rule 6).
	// Without this, bridge_service_users.x25519_pubkey stays NULL and
	// the cert fan-out skips the bridge → 465/993 get no cert.
	if c := m.calls(wsrpc.MethodRegisterServiceUser); c < 1 {
		t.Fatalf("register_service_user calls = %d, want >= 1 (x25519 attestation)", c)
	}
	wantX := expectedX25519Pubkey(t, keypairPath)
	gotEd, gotX := m.attested()
	if !bytes.Equal(gotX, wantX) {
		t.Fatalf("attested x25519 = %x, want %x (keyfile-derived)", gotX, wantX)
	}
	if len(gotEd) != 32 {
		t.Fatalf("attested ed25519 length = %d, want 32", len(gotEd))
	}

	// /metrics serves and exposes the wsrpc counters. The counter's
	// `method` label is the raw RPC kind string passed to Client.Call
	// (the Go wrapper names like "Whoami" never reach the metric), so
	// the expected label is method="fauna.bridges.<kind>".
	body := scrapeMetrics(t, metricsAddr)
	for _, want := range []string{
		"wsrpc_calls_total",
		"wsrpc_call_seconds",
		fmt.Sprintf("method=%q", wsrpc.MethodWhoami),
		fmt.Sprintf("method=%q", wsrpc.MethodFetchConfig),
		fmt.Sprintf("method=%q", wsrpc.MethodFetchTLSCertBlob),
		fmt.Sprintf("method=%q", wsrpc.MethodRegisterServiceUser),
	} {
		if !strings.Contains(body, want) {
			t.Errorf("/metrics body missing %q\nbody:\n%s", want, body)
		}
	}

	// SIGTERM → clean exit.
	err := bp.sigtermAndWait(t, 5*time.Second)
	if err != nil {
		t.Fatalf("SIGTERM exit: %v\nstderr:\n%s", err, bp.stderr.String())
	}
}

// TestBridgeIdlesWhenMailDisabled — Whoami says role=mta but the
// ConfigSnapshot is all-zero (mail off per the Phase B.8
// heuristic). The bridge must still enroll, expose /metrics, and
// idle gracefully without crashing.
//
// The Whoami.Domain is also empty in this test — that exercises
// the "no domain → skip TLS-cert fetch" path. So
// fetch_tls_cert_blob must NOT be called in this case, and the
// bridge must still come up.
func TestBridgeIdlesWhenMailDisabled(t *testing.T) {
	integrationGate(t)

	tmpDir := t.TempDir()
	keypairPath := writeTestKeyfile(t, tmpDir)
	metricsAddr := freeTCPAddr(t)
	// MTA listeners aren't reached on this disabled-path test (Run
	// idles on the MailEnabled gate before binding), but we still
	// pin free ports to keep the operator-hatch shape consistent
	// across MTA-role tests.
	mtaAddr := freeTCPAddr(t)
	mtaAddr465 := freeTCPAddr(t)
	mtaAddr587 := freeTCPAddr(t)
	makeOperatorHatch(t, tmpDir, metricsAddr, mtaAddr, mtaAddr465, mtaAddr587)

	whoami := wsrpc.WhoamiReply{
		Role:             "mta",
		BridgeID:         "integration-mta-disabled",
		Status:           "approved",
		Ed25519PubkeyHex: strings.Repeat("ee", 32),
		X25519PubkeyHex:  strings.Repeat("ff", 32),
	}
	// disabledConfigSnapshot() returns an all-zero snapshot —
	// MailEnabled=false AND empty LocalDomains/PrimaryDomain →
	// bridge defers TLS-cert fetch on the primary_domain==""
	// gate before reaching the MailEnabled idle path.
	m := startMockNest(t, whoami, disabledConfigSnapshot(), nil)

	bin := buildBridgeBinary(t)
	bp := startBridge(t, bin, keypairPath, m.srv.URL, tmpDir)
	bp.waitForReady(t, 10*time.Second)

	// Whoami + FetchConfig happened; TLS-cert fetch did NOT (because
	// Domain is empty). This is the load-bearing assertion for the
	// default-off path — the bridge must skip domain-keyed RPCs.
	if c := m.calls(wsrpc.MethodWhoami); c < 1 {
		t.Fatalf("whoami calls = %d, want >= 1", c)
	}
	if c := m.calls(wsrpc.MethodFetchConfig); c < 1 {
		t.Fatalf("fetch_config calls = %d, want >= 1", c)
	}
	if c := m.calls(wsrpc.MethodFetchTLSCertBlob); c != 0 {
		t.Fatalf("fetch_tls_cert_blob calls = %d, want 0 (empty domain → fetch deferred)", c)
	}

	// /metrics still serves.
	body := scrapeMetrics(t, metricsAddr)
	if !strings.Contains(body, "wsrpc_calls_total") {
		t.Errorf("/metrics body missing wsrpc_calls_total\nbody:\n%s", body)
	}

	// The role-stub idles cleanly through the disabled path.
	err := bp.sigtermAndWait(t, 5*time.Second)
	if err != nil {
		t.Fatalf("SIGTERM exit: %v\nstderr:\n%s", err, bp.stderr.String())
	}
}

// TestBridgeSurvivesMetricsBindFailure — Stage-5 Gap D, non-fatal
// metrics bind. The test process holds the bridge's metrics port for
// the test's lifetime, so the bridge's /metrics listener fails to bind.
// The bridge must still reach "ready" and exit cleanly on SIGTERM
// rather than crash-loop — /metrics is observability-only, so its
// failure must never strand mail serving (this is also what stops the
// two same-host roles from killing each other when they would collide).
func TestBridgeSurvivesMetricsBindFailure(t *testing.T) {
	integrationGate(t)

	tmpDir := t.TempDir()
	keypairPath := writeTestKeyfile(t, tmpDir)

	// Occupy the metrics port for the whole test so the bridge's bind
	// fails (address already in use). Unlike freeTCPAddr (which closes
	// the socket), we keep the listener open.
	held, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("hold metrics port: %v", err)
	}
	defer held.Close()
	metricsAddr := held.Addr().String()

	// Disabled snapshot → MTA idles without binding privileged ports;
	// we only care the process survives the metrics bind failure. The
	// MTA listener addrs are still pinned to keep the hatch shape.
	mtaAddr := freeTCPAddr(t)
	mtaAddr465 := freeTCPAddr(t)
	mtaAddr587 := freeTCPAddr(t)
	makeOperatorHatch(t, tmpDir, metricsAddr, mtaAddr, mtaAddr465, mtaAddr587)

	whoami := wsrpc.WhoamiReply{
		Role:             "mta",
		BridgeID:         "integration-mta-metricsfail",
		Status:           "approved",
		Ed25519PubkeyHex: strings.Repeat("ee", 32),
		X25519PubkeyHex:  strings.Repeat("ff", 32),
	}
	m := startMockNest(t, whoami, disabledConfigSnapshot(), nil)

	bin := buildBridgeBinary(t)
	bp := startBridge(t, bin, keypairPath, m.srv.URL, tmpDir)
	// Must reach "ready" despite the failed metrics bind. On the
	// pre-fix (fatal) code path the process exits here instead.
	bp.waitForReady(t, 10*time.Second)

	// The non-fatal handler logs the failure but keeps the process up.
	if !strings.Contains(bp.stderr.String(), "metrics listener failed") {
		t.Errorf("expected a non-fatal metrics-failure log line; stderr:\n%s", bp.stderr.String())
	}

	// The process was healthy (not crash-looping) → SIGTERM → exit 0.
	if err := bp.sigtermAndWait(t, 5*time.Second); err != nil {
		t.Fatalf("SIGTERM exit: %v\nstderr:\n%s", err, bp.stderr.String())
	}
}
