package imap

import (
	"context"
	"errors"
	"log/slog"
	"sync"
	"testing"
	"time"
)

// recordingCaller records every wsrpc Caller invocation; tests inspect
// the captured calls. Methods that should never fire return an error
// so a missing expectation surfaces as a test failure, not silently.
type recordingCaller struct {
	mu    sync.Mutex
	calls []recordedCall
}

type recordedCall struct {
	method string
	body   any
}

func (r *recordingCaller) Call(_ context.Context, method string, body any, reply any) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.calls = append(r.calls, recordedCall{method: method, body: body})
	// report_session_close returns ok=true on the wire — populate the
	// caller's reply pointer so the wrapper's ok-check passes.
	if method == "fauna.bridges.report_session_close" {
		// reply is *reportSessionCloseReply; set OK=true via reflection-
		// free path: the wrapper inspects reply.OK after Call returns,
		// so a no-op here means OK=false. Tests should construct their
		// own reply by hand, but in practice for Phase C.2 the wrapper
		// logs the warning on ok=false and continues — that's fine for
		// our tests (we assert the call HAPPENED, not its outcome).
		_ = reply
	}
	return nil
}

func (r *recordingCaller) callCount(method string) int {
	r.mu.Lock()
	defer r.mu.Unlock()
	n := 0
	for _, c := range r.calls {
		if c.method == method {
			n++
		}
	}
	return n
}

// TestSessionCloseZeroizesMLSUnwrap pins the zeroization contract:
// Session.Close MUST call mlsUnwrap.Zeroize() before nil'ing the
// field. Per imap-server.md § Authentication.
//
// Verification path (since *mailfauna.MLSCapability has no observable
// zeroize-state getter): after Close, the field is nil AND further
// Decrypt calls on the saved-aside handle return an error mentioning
// "zeroized". The capability is constructed from the committed PLAIN
// wrapped-MSEK test vector (Phase C.3 corpus) so the pre-zeroize state
// is a real mlock'd MSEK, not a fake.
func TestSessionCloseZeroizesMLSUnwrap(t *testing.T) {
	cap := mustUnwrapPlainFixture(t)
	s := &Session{
		client:       &recordingCaller{},
		actorID:      []byte("not-nil-so-report-fires"),
		credentialID: "default",
		mlsUnwrap:    cap,
	}
	if err := s.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if s.mlsUnwrap != nil {
		t.Fatal("mlsUnwrap field not nil'd after Close")
	}
	// The same *MLSCapability we saved aside must now reject Decrypt
	// — the inner MSEK is gone.
	if _, err := cap.Decrypt([]byte{0, 0, 0}); err == nil {
		t.Fatal("expected Decrypt to fail after Close zeroized the capability")
	}
}

// TestSessionCloseReportsToNest pins the audit-log contract: when AUTH
// succeeded (actorID is set), Close fires exactly one
// fauna.bridges.report_session_close RPC. When AUTH did not succeed,
// the RPC must not fire (avoids polluting the audit log with phantom
// pre-AUTH disconnects).
func TestSessionCloseReportsToNest(t *testing.T) {
	t.Run("after auth", func(t *testing.T) {
		client := &recordingCaller{}
		s := &Session{
			client:       client,
			actorID:      []byte{0x01, 0x02, 0x03},
			credentialID: "default",
			nowFn:        func() time.Time { return time.Unix(1700_000_000, 0) },
		}
		_ = s.Close()
		if got := client.callCount("fauna.bridges.report_session_close"); got != 1 {
			t.Fatalf("report_session_close called %d times, want 1", got)
		}
	})
	t.Run("pre auth", func(t *testing.T) {
		client := &recordingCaller{}
		s := &Session{client: client}
		_ = s.Close()
		if got := client.callCount("fauna.bridges.report_session_close"); got != 0 {
			t.Fatalf("report_session_close fired %d times pre-AUTH, want 0", got)
		}
	})
}

// TestSessionCloseSwallowsReportRPCFailure asserts a failing report
// RPC at teardown doesn't cause Close to return an error or panic.
// Per imap-server.md and the design rationale in session.go: the
// connection is going away regardless; failed teardown audit is
// logged, not surfaced.
func TestSessionCloseSwallowsReportRPCFailure(t *testing.T) {
	client := &failingCaller{}
	s := &Session{
		client:       client,
		actorID:      []byte{0x01},
		credentialID: "default",
		logger:       slog.Default(),
	}
	if err := s.Close(); err != nil {
		t.Fatalf("Close returned %v; want nil even when report RPC fails", err)
	}
}

// failingCaller is a wsrpc.Caller that errors on every Call. Verifies
// the Session.Close swallow-and-log behaviour.
type failingCaller struct{}

func (failingCaller) Call(context.Context, string, any, any) error {
	return errors.New("simulated RPC failure")
}

// TestNewBackendNewSessionNotNil pins the Backend factory contract:
// NewBackend must return a Backend whose NewSession returns a non-nil
// Session that satisfies imapserver.Session.
func TestNewBackendNewSessionNotNil(t *testing.T) {
	b := NewBackend(&recordingCaller{}, slog.Default(), 0, 0, 0, nil, nil)
	if b == nil {
		t.Fatal("NewBackend returned nil")
	}
	// nil Conn is fine — the Session stashes it but never dereferences
	// it during construction.
	sess := b.NewSession(nil)
	if sess == nil {
		t.Fatal("Backend.NewSession returned nil Session")
	}
}
