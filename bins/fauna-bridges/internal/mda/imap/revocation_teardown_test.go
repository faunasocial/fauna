package imap

import (
	"bufio"
	"context"
	"crypto/tls"
	"encoding/base64"
	"encoding/hex"
	"io"
	"log/slog"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// teardownCaller is the nest across a whole IMAP session: AUTH through
// authCaller, SELECT through selectCaller, the IDLE subscription through
// idleSubscribeCaller.
type teardownCaller struct {
	auth *authCaller
	sel  *selectCaller
	idle *idleSubscribeCaller
}

func (c *teardownCaller) Call(ctx context.Context, method string, body, reply any) error {
	switch method {
	case wsrpc.MethodSelectMailbox, wsrpc.MethodListMessages, wsrpc.MethodFetchMessageMetadata:
		return c.sel.Call(ctx, method, body, reply)
	case "fauna.bridges.subscribe_mailbox_state":
		return c.idle.Call(ctx, method, body, reply)
	default:
		return c.auth.Call(ctx, method, body, reply)
	}
}

// signedInSession stands up an MDA IMAP server whose pushes arrive through a
// real PushDispatcher, signs a mail app in as the fixture actor and SELECTs a
// mailbox. Returns the connection, its reader and the dispatcher.
func signedInSession(t *testing.T) (net.Conn, *bufio.Reader, *wsrpc.PushDispatcher) {
	t.Helper()
	c := &teardownCaller{
		auth: &authCaller{
			validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
			blobBytes:                 mustReadFixture(t, "wrapped_msek.bin"),
			mlsPubkey:                 fixtureMLSPubkey,
		},
		sel: &selectCaller{reply: map[string]any{
			"outcome":       "selected",
			"uid_validity":  uint32(7),
			"uid_next":      uint32(1),
			"highestmodseq": int64(1),
			"exists":        uint32(0),
			"recent":        uint32(0),
			"unseen":        uint32(0),
		}},
		idle: &idleSubscribeCaller{subscriptionID: 77},
	}
	dispatcher := wsrpc.NewPushDispatcher(slog.Default())
	listener := newTLSListener(t, selfSignedCert(t))
	t.Cleanup(func() { _ = listener.Close() })
	srv := NewServer(ServerConfig{}, NewBackend(c, slog.Default(), 0, 0, 0, dispatcher, nil))
	go func() { _ = srv.Serve(listener) }()
	t.Cleanup(func() { _ = srv.Close() })

	conn, err := tls.Dial("tcp", listener.Addr().String(), &tls.Config{InsecureSkipVerify: true})
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
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
	// Not INBOX, so the SELECT-time spam pass stays out of the way.
	if _, err := io.WriteString(conn, "A002 SELECT Archive\r\n"); err != nil {
		t.Fatalf("write SELECT: %v", err)
	}
	readTagged(t, r, "A002 ", "A002 OK")
	return conn, r, dispatcher
}

func pushAccountRevoked(t *testing.T, d *wsrpc.PushDispatcher, actorID []byte) {
	t.Helper()
	payload, err := dagcbor.Marshal(wsrpc.BridgeAccountRevokedPush{ActorID: actorID})
	if err != nil {
		t.Fatalf("marshal push: %v", err)
	}
	d.Handle(wsrpc.PushKindAccountRevoked, payload, 1)
}

// TestAnIdleSessionEndsAtTheAccountRevokedPush — the IDLE
// half of the bridge-held revocation teardown (`transport-connection.md`
// § Revocation teardown): a mail app idling when the account is locked sends
// nothing more, so the next-command refusal never reaches it. The nest's
// `account_revoked` push does: the session answers `* BYE` and the connection
// closes, with no command from the client.
//
// Mutation pinned: dropping the dispatcher registration, the IDLE loop's
// revoked arm, or the AUTH-time join of the live-session index leaves the
// session idling until the read deadline — the test fails on "no BYE".
func TestAnIdleSessionEndsAtTheAccountRevokedPush(t *testing.T) {
	conn, r, dispatcher := signedInSession(t)
	if _, err := io.WriteString(conn, "A003 IDLE\r\n"); err != nil {
		t.Fatalf("write IDLE: %v", err)
	}
	if line, err := r.ReadString('\n'); err != nil || !strings.HasPrefix(line, "+") {
		t.Fatalf("want the '+ idling' continuation, got %q (%v)", line, err)
	}

	// Another actor's revocation leaves this session idling.
	pushAccountRevoked(t, dispatcher, make([]byte, 32))
	_ = conn.SetReadDeadline(time.Now().Add(300 * time.Millisecond))
	if line, err := r.ReadString('\n'); err == nil {
		t.Fatalf("another account's revocation reached this session: %q", strings.TrimSpace(line))
	}

	// The owner presses the lock; the nest pushes. The client sends nothing.
	_ = conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	pushAccountRevoked(t, dispatcher, fixtureActorID)
	expectByeThenClose(t, r)
}

// TestASessionBetweenCommandsEndsAtTheAccountRevokedPush: a signed-in session
// that is not idling — the mail app holds the connection open between
// commands — is disconnected at the push, too, rather than at its next
// command.
func TestASessionBetweenCommandsEndsAtTheAccountRevokedPush(t *testing.T) {
	conn, r, dispatcher := signedInSession(t)
	pushAccountRevoked(t, dispatcher, fixtureActorID)
	_ = conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			if ne, ok := err.(net.Error); ok && ne.Timeout() {
				t.Fatal("the session stayed open after its account was revoked")
			}
			return // closed — the session ended
		}
		if !strings.HasPrefix(line, "* BYE") {
			t.Fatalf("got %q: a revoked session must say nothing but BYE", strings.TrimSpace(line))
		}
	}
}

// expectByeThenClose reads until the connection closes, requiring a BYE
// (untagged or tagged) and nothing after it.
func expectByeThenClose(t *testing.T, r *bufio.Reader) {
	t.Helper()
	sawBye := false
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			if ne, ok := err.(net.Error); ok && ne.Timeout() {
				t.Fatalf("no close within the deadline (BYE seen: %v)", sawBye)
			}
			if !sawBye {
				t.Fatalf("connection ended with no BYE: %v", err)
			}
			return
		}
		if sawBye {
			t.Fatalf("got %q after the BYE: the connection must close", strings.TrimSpace(line))
		}
		if strings.HasPrefix(line, "* BYE") || strings.HasPrefix(line, "A003 BYE") {
			sawBye = true
			continue
		}
		t.Fatalf("got %q: an idling session of a revoked account must end with a BYE", strings.TrimSpace(line))
	}
}
