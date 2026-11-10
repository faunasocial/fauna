package imap

import (
	"bufio"
	"context"
	"crypto/tls"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"io"
	"log/slog"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
)

// lockableCaller is the nest as the MDA sees it across an account lock: it
// answers as inner until locked, then refuses every call with
// wsrpc.CodeAccountRevoked, the refusal `ensure_target_stands` returns.
type lockableCaller struct {
	inner  wsrpc.Caller
	locked atomic.Bool
}

func (c *lockableCaller) Call(ctx context.Context, method string, body any, reply any) error {
	if c.locked.Load() && method != wsrpc.MethodReportSessionClose {
		return accountRevokedServerError()
	}
	return c.inner.Call(ctx, method, body, reply)
}

func accountRevokedServerError() error {
	payload, err := cbor.Marshal(map[string]any{"code": wsrpc.CodeAccountRevoked})
	if err != nil {
		panic(err)
	}
	return &wsrpc.ServerError{Payload: payload}
}

// TestAnOpenSessionEndsAtTheAccountLock — the bridge-held
// half of the revocation teardown: a mail app signed in before the account
// was locked is disconnected at its next command. The nest refuses the call
// (`account_revoked`); the session answers `* BYE` and the connection closes,
// rather than a `NO` that leaves it open.
func TestAnOpenSessionEndsAtTheAccountLock(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	c := &lockableCaller{inner: &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
	}}
	listener := newTLSListener(t, selfSignedCert(t))
	defer listener.Close()
	srv := NewServer(ServerConfig{}, NewBackend(c, slog.Default(), 0, 0, 0, nil, nil))
	go func() { _ = srv.Serve(listener) }()
	defer func() { _ = srv.Close() }()

	conn, err := tls.Dial("tcp", listener.Addr().String(), &tls.Config{InsecureSkipVerify: true})
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer conn.Close()
	_ = conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	r := bufio.NewReader(conn)
	if _, err := r.ReadString('\n'); err != nil {
		t.Fatalf("read greeting: %v", err)
	}
	if _, err := io.WriteString(conn, "A001 AUTHENTICATE PLAIN\r\n"); err != nil {
		t.Fatalf("write AUTHENTICATE: %v", err)
	}
	if line, err := r.ReadString('\n'); err != nil || !strings.HasPrefix(line, "+") {
		t.Fatalf("want a '+' continuation, got %q (%v)", line, err)
	}
	resp := base64.StdEncoding.EncodeToString([]byte(plainPayload("", "alice@example.com", string(fixturePlainPassword))))
	if _, err := io.WriteString(conn, resp+"\r\n"); err != nil {
		t.Fatalf("write PLAIN response: %v", err)
	}
	readTagged(t, r, "A001 ", "A001 OK")

	// The owner presses the lock.
	c.locked.Store(true)

	if _, err := io.WriteString(conn, "A002 LIST \"\" \"*\"\r\n"); err != nil {
		t.Fatalf("write LIST: %v", err)
	}
	sawBye := false
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			if !sawBye {
				t.Fatalf("connection ended with no BYE: %v", err)
			}
			return // closed after the BYE — the session ended
		}
		if sawBye {
			t.Fatalf("got %q after the BYE: the connection must close", strings.TrimSpace(line))
		}
		// The BYE may come untagged or carry the command's tag; either way
		// it is the last thing the server says on this connection.
		if strings.HasPrefix(line, "* BYE") || strings.HasPrefix(line, "A002 BYE") {
			sawBye = true
			continue
		}
		if strings.HasPrefix(line, "A002 ") {
			t.Fatalf("got %q: a locked account's session must end with a BYE, not answer the command", strings.TrimSpace(line))
		}
	}
}

// TestRevokedByeCallerKeepsTheRefusalReachable: the wrapped error is a BYE to
// the IMAP server and still the nest's typed refusal to every code reader;
// every other error passes through untouched.
func TestRevokedByeCallerKeepsTheRefusalReachable(t *testing.T) {
	c := revokedByeCaller{inner: &lockableCaller{inner: &recordingCaller{}}}
	c.inner.(*lockableCaller).locked.Store(true)
	err := c.Call(context.Background(), wsrpc.MethodListMailboxes, nil, nil)
	var imapErr *imap.Error
	if !errors.As(err, &imapErr) || imapErr.Type != imap.StatusResponseTypeBye {
		t.Fatalf("err = %v, want an IMAP BYE", err)
	}
	if code, ok := wsrpc.RpcErrorCode(err); !ok || code != wsrpc.CodeAccountRevoked {
		t.Fatalf("RpcErrorCode = %q, %v; want %s", code, ok, wsrpc.CodeAccountRevoked)
	}

	other := errors.New("transport down")
	got := revokedByeCaller{inner: erroringCaller{err: other}}.Call(context.Background(), "x", nil, nil)
	if !errors.Is(got, other) || errors.As(got, &imapErr) {
		t.Fatalf("a non-revocation error must pass through untouched, got %v", got)
	}
}

type erroringCaller struct{ err error }

func (f erroringCaller) Call(context.Context, string, any, any) error { return f.err }
