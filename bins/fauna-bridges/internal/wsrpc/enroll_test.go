// Tests for the zero-touch self-enrollment poll loop (enroll.go).
//
// These exercise PollEnrollmentUntilApproved against a fake Caller — no WS
// server, no real time — asserting the cold-boot state machine: pending keeps
// polling on the backoff curve (without re-dialing), approved proceeds, revoked
// → ErrBridgeRevoked, a ctx cancel exits cleanly, and an RPC error is fatal.
package wsrpc

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"errors"
	"io"
	"log/slog"
	"testing"
	"time"
)

// TestEnrollmentSignedMessage_layout pins the exact byte layout of the slice-2
// proof-of-possession message so the Go mirror can't drift from the Rust single
// source (fauna_protocol::wrapped_blob::enrollment_signed_message). A drift here
// silently breaks live enrollment — nest reconstructs different bytes and the
// signature fails to verify — with no compile error to catch it.
func TestEnrollmentSignedMessage_layout(t *testing.T) {
	ed := bytes.Repeat([]byte{0x11}, 32)
	x := bytes.Repeat([]byte{0x22}, 32)
	const domain = "fauna.bridges.enroll.v1"

	msg := EnrollmentSignedMessage("mta", ed, x)
	// domain tag ‖ ed25519(32) ‖ x25519(32) ‖ role
	want := append([]byte(domain), ed...)
	want = append(want, x...)
	want = append(want, "mta"...)
	if !bytes.Equal(msg, want) {
		t.Fatalf("layout mismatch:\n got %x\nwant %x", msg, want)
	}
	if !bytes.HasPrefix(msg, []byte(domain)) {
		t.Errorf("message must start with the domain tag %q", domain)
	}
	// Field-bound: role, ed25519, and x25519 each change the bytes.
	if bytes.Equal(msg, EnrollmentSignedMessage("mda", ed, x)) {
		t.Errorf("role must bind the message (mta vs mda produced equal bytes)")
	}
	if bytes.Equal(msg, EnrollmentSignedMessage("mta", bytes.Repeat([]byte{0x99}, 32), x)) {
		t.Errorf("ed25519 pubkey must bind the message")
	}
	if bytes.Equal(msg, EnrollmentSignedMessage("mta", ed, bytes.Repeat([]byte{0x99}, 32))) {
		t.Errorf("x25519 pubkey must bind the message")
	}
}

// TestSignEnrollment_verifies confirms SignEnrollment produces a signature that
// verifies against the same single-source message with the corresponding pubkey
// — the exact check nest's check_enrollment_authorization performs.
func TestSignEnrollment_verifies(t *testing.T) {
	edPub, edPriv, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatalf("GenerateKey: %v", err)
	}
	x := bytes.Repeat([]byte{0x33}, 32)
	sig := SignEnrollment(edPriv, "mda", edPub, x)
	if len(sig) != ed25519.SignatureSize {
		t.Fatalf("sig length: got %d, want %d", len(sig), ed25519.SignatureSize)
	}
	if !ed25519.Verify(edPub, EnrollmentSignedMessage("mda", edPub, x), sig) {
		t.Errorf("SignEnrollment signature does not verify against the single-source message")
	}
	// A signature over a different role must NOT verify under the mda message.
	bad := SignEnrollment(edPriv, "mta", edPub, x)
	if ed25519.Verify(edPub, EnrollmentSignedMessage("mda", edPub, x), bad) {
		t.Errorf("a role-mismatched signature verified; the role must be bound")
	}
}

// enrollCaller is a fake Caller returning a scripted sequence of enrollment
// statuses (the last entry repeats once exhausted). If err is set it is
// returned once the scripted statuses run out (or immediately when statuses is
// empty), modelling a mid-wait connection error. It rejects any method other
// than request_enrollment so a wiring mistake is caught.
type enrollCaller struct {
	statuses []string
	err      error
	calls    int
}

func (c *enrollCaller) Call(_ context.Context, method string, _, reply any) error {
	c.calls++
	if method != MethodRequestEnrollment {
		return errors.New("enrollCaller: unexpected method " + method)
	}
	if c.calls > len(c.statuses) {
		if c.err != nil {
			return c.err
		}
		// Repeat the last scripted status (pending-forever cases).
	}
	idx := c.calls - 1
	if idx >= len(c.statuses) {
		idx = len(c.statuses) - 1
	}
	rep, ok := reply.(*requestEnrollmentReply)
	if !ok {
		return errors.New("enrollCaller: reply is not *requestEnrollmentReply")
	}
	rep.Status = c.statuses[idx]
	return nil
}

func discardLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}

func TestPollEnrollmentUntilApproved_PendingThenApproved(t *testing.T) {
	caller := &enrollCaller{statuses: []string{StatusPending, StatusPending, StatusApproved}}
	var sleeps []time.Duration
	opts := EnrollPollOptions{
		// Distinct, fast delays so we can assert the curve index without real waits.
		Backoff: func(n int) time.Duration { return time.Duration(n+1) * time.Millisecond },
		sleep: func(_ context.Context, d time.Duration) error {
			sleeps = append(sleeps, d)
			return nil
		},
		Logger: discardLogger(),
	}
	err := PollEnrollmentUntilApproved(context.Background(), caller, EnrollmentIdentity{Ed25519Pub: []byte("pk"), RoleHint: "mta"}, opts)
	if err != nil {
		t.Fatalf("PollEnrollmentUntilApproved: %v", err)
	}
	if caller.calls != 3 {
		t.Errorf("request_enrollment calls = %d, want 3 (pending, pending, approved)", caller.calls)
	}
	// Two pending replies → two backoff sleeps at indices 0 and 1.
	want := []time.Duration{1 * time.Millisecond, 2 * time.Millisecond}
	if len(sleeps) != len(want) {
		t.Fatalf("sleeps = %v, want %v", sleeps, want)
	}
	for i := range want {
		if sleeps[i] != want[i] {
			t.Errorf("sleep[%d] = %v, want %v (backoff curve must advance by attempt index)", i, sleeps[i], want[i])
		}
	}
}

func TestPollEnrollmentUntilApproved_Revoked(t *testing.T) {
	caller := &enrollCaller{statuses: []string{StatusRevoked}}
	err := PollEnrollmentUntilApproved(context.Background(), caller, EnrollmentIdentity{Ed25519Pub: []byte("pk"), RoleHint: "mda"}, EnrollPollOptions{
		sleep: func(context.Context, time.Duration) error {
			t.Fatal("should not sleep on a revoked terminal status")
			return nil
		},
		Logger: discardLogger(),
	})
	if !errors.Is(err, ErrBridgeRevoked) {
		t.Fatalf("err = %v, want ErrBridgeRevoked", err)
	}
	if caller.calls != 1 {
		t.Errorf("calls = %d, want 1 (revoked is terminal on the first poll)", caller.calls)
	}
}

func TestPollEnrollmentUntilApproved_UnknownStatusKeepsPolling(t *testing.T) {
	// A non-pending/non-terminal status ("unknown") must not exit — keep polling.
	caller := &enrollCaller{statuses: []string{"unknown", StatusApproved}}
	err := PollEnrollmentUntilApproved(context.Background(), caller, EnrollmentIdentity{Ed25519Pub: []byte("pk"), RoleHint: "mta"}, EnrollPollOptions{
		Backoff: func(int) time.Duration { return time.Microsecond },
		sleep:   func(context.Context, time.Duration) error { return nil },
		Logger:  discardLogger(),
	})
	if err != nil {
		t.Fatalf("PollEnrollmentUntilApproved: %v", err)
	}
	if caller.calls != 2 {
		t.Errorf("calls = %d, want 2 (unknown keeps polling, then approved)", caller.calls)
	}
}

func TestPollEnrollmentUntilApproved_CtxCancelExitsClean(t *testing.T) {
	caller := &enrollCaller{statuses: []string{StatusPending}} // pending forever
	err := PollEnrollmentUntilApproved(context.Background(), caller, EnrollmentIdentity{Ed25519Pub: []byte("pk"), RoleHint: "mta"}, EnrollPollOptions{
		Backoff: func(int) time.Duration { return time.Millisecond },
		// Model a SIGTERM arriving during the inter-poll wait.
		sleep:  func(context.Context, time.Duration) error { return context.Canceled },
		Logger: discardLogger(),
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}
	if caller.calls != 1 {
		t.Errorf("calls = %d, want 1 (cancelled during the first wait)", caller.calls)
	}
}

func TestPollEnrollmentUntilApproved_RPCErrorIsFatal(t *testing.T) {
	// A connection/RPC error (e.g. the anonymous WS dropping) is fatal — the
	// caller exits and s6 restarts the bridge, which re-enrolls idempotently.
	caller := &enrollCaller{statuses: nil, err: ErrClosed}
	err := PollEnrollmentUntilApproved(context.Background(), caller, EnrollmentIdentity{Ed25519Pub: []byte("pk"), RoleHint: "mta"}, EnrollPollOptions{
		sleep: func(context.Context, time.Duration) error {
			t.Fatal("should not sleep after a fatal RPC error")
			return nil
		},
		Logger: discardLogger(),
	})
	if !errors.Is(err, ErrClosed) {
		t.Fatalf("err = %v, want it to wrap ErrClosed", err)
	}
	if caller.calls != 1 {
		t.Errorf("calls = %d, want 1", caller.calls)
	}
}
