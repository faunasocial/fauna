package imap

import (
	"bufio"
	"crypto/tls"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"io"
	"log/slog"
	"net"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/connlimit"
)

// deadlineRecorder sits BELOW the pre-auth wrapper and records every read
// deadline that reaches the real socket — what the fork can actually be held
// to, observed without reading the fork.
type deadlineRecorder struct {
	net.Conn
	mu   sync.Mutex
	read []time.Time
}

func (c *deadlineRecorder) SetReadDeadline(t time.Time) error {
	c.mu.Lock()
	c.read = append(c.read, t)
	c.mu.Unlock()
	return c.Conn.SetReadDeadline(t)
}

func (c *deadlineRecorder) SetDeadline(t time.Time) error {
	if err := c.Conn.SetWriteDeadline(t); err != nil {
		return err
	}
	return c.SetReadDeadline(t)
}

func (c *deadlineRecorder) snapshot() []time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]time.Time(nil), c.read...)
}

type recordingListener struct {
	net.Listener
	accepted chan *deadlineRecorder
}

func (l *recordingListener) Accept() (net.Conn, error) {
	c, err := l.Listener.Accept()
	if err != nil {
		return nil, err
	}
	rec := &deadlineRecorder{Conn: c}
	l.accepted <- rec
	return rec, nil
}

// preAuthStack builds the production 993 shape over a recorder —
// tls.NewListener(NewPreAuthListener(recorder(raw))) — or, with tlsOn false,
// the 143 shape without TLS.
func preAuthStack(t *testing.T, tlsOn bool, now func() time.Time, onDeadline func()) (net.Listener, *recordingListener) {
	t.Helper()
	raw, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	rec := &recordingListener{Listener: raw, accepted: make(chan *deadlineRecorder, 4)}
	pl := connlimit.NewPreAuthListener(rec, onDeadline)
	pl.SetClockForTest(now)
	if !tlsOn {
		return pl, rec
	}
	return tls.NewListener(pl, &tls.Config{Certificates: []tls.Certificate{selfSignedCert(t)}}), rec
}

// TestPreAuthDeadlineLiftedByAuthenticatePLAIN: on the 993 shape with the
// real fork and the real Session, every read deadline that reaches the socket
// is held to the pre-auth bound until AUTHENTICATE PLAIN succeeds, and one
// past the bound reaches it after — no sleep, no fork read. The injected
// accept time sits 50 s in the past, so the bound (real now + 10 s, the same
// ceiling as the client's own read deadline) is earlier than any deadline the
// fork pokes per command: pre-auth the clamp shows, post-auth the lift does.
func TestPreAuthDeadlineLiftedByAuthenticatePLAIN(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	c := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
	}
	accept := time.Now().Add(-50 * time.Second)
	bound := accept.Add(connlimit.PreAuthDeadline)
	listener, rec := preAuthStack(t, true, func() time.Time { return accept }, nil)
	defer listener.Close()
	srv := NewServer(ServerConfig{}, NewBackend(c, slog.Default(), 0, 0, 1, nil, nil))
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
	sock := <-rec.accepted

	if _, err := io.WriteString(conn, "A001 AUTHENTICATE PLAIN\r\n"); err != nil {
		t.Fatalf("write AUTHENTICATE: %v", err)
	}
	if line, err := r.ReadString('\n'); err != nil || !strings.HasPrefix(line, "+") {
		t.Fatalf("want a '+' continuation, got %q (%v)", line, err)
	}
	// The fork has read the AUTHENTICATE line: every deadline so far — the
	// accept's and the fork's own per-command pokes — is the bound.
	pre := sock.snapshot()
	if len(pre) < 2 {
		t.Fatalf("pre-auth socket deadlines = %s, want accept's and at least one fork poke", fmtDeadlines(pre, accept))
	}
	for _, d := range pre {
		if !d.Equal(bound) {
			t.Fatalf("pre-auth socket deadlines = %s, want every one held to the bound accept+%v", fmtDeadlines(pre, accept), connlimit.PreAuthDeadline)
		}
	}

	resp := base64.StdEncoding.EncodeToString([]byte(plainPayload("", "alice@example.com", string(fixturePlainPassword))))
	if _, err := io.WriteString(conn, resp+"\r\n"); err != nil {
		t.Fatalf("write PLAIN response: %v", err)
	}
	readTagged(t, r, "A001 ", "A001 OK")
	if _, err := io.WriteString(conn, "A002 NOOP\r\n"); err != nil {
		t.Fatalf("write NOOP: %v", err)
	}
	readTagged(t, r, "A002 ", "A002 OK")
	// The fork has read a post-auth command: its own deadline now reaches the
	// socket unclamped.
	post := sock.snapshot()
	if last := post[len(post)-1]; !last.IsZero() && !last.After(bound) {
		t.Fatalf("post-auth socket deadlines = %s, want the last one cleared or past the bound accept+%v", fmtDeadlines(post, accept), connlimit.PreAuthDeadline)
	}
}

// TestPreAuthDeadlineClosesAnIdleStranger: with the accept reading a time
// already past the bound, a client that sends nothing after the greeting
// reads EOF at once — a real close, no wall-clock wait — counted once.
func TestPreAuthDeadlineClosesAnIdleStranger(t *testing.T) {
	var fired atomic.Int32
	// Only the accept reads the past; every later read of the clock is the
	// real one, so the timeout is seen as ours.
	var accepted atomic.Bool
	past := func() time.Time {
		if accepted.CompareAndSwap(false, true) {
			return time.Now().Add(-2 * connlimit.PreAuthDeadline)
		}
		return time.Now()
	}
	listener, _ := preAuthStack(t, false, past, func() { fired.Add(1) })
	defer listener.Close()
	srv := NewServer(ServerConfig{}, NewBackend(&authCaller{}, slog.Default(), 0, 0, 1, nil, nil))
	go func() { _ = srv.Serve(listener) }()
	defer func() { _ = srv.Close() }()

	conn, err := net.Dial("tcp", listener.Addr().String())
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer conn.Close()
	_ = conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	r := bufio.NewReader(conn)
	if line, err := r.ReadString('\n'); err != nil || !strings.HasPrefix(line, "* ") {
		t.Fatalf("want the greeting, got %q (%v)", line, err)
	}
	for {
		line, err := r.ReadString('\n')
		if err == io.EOF {
			break
		}
		if err != nil {
			t.Fatalf("want EOF from the server's close, got %v", err)
		}
		// An untagged BYE before the close is allowed; nothing else is.
		if !strings.HasPrefix(line, "* BYE") {
			t.Fatalf("unexpected line before close: %q", line)
		}
	}
	if got := fired.Load(); got != 1 {
		t.Fatalf("preauth_deadline counted %d times, want 1", got)
	}
}

func readTagged(t *testing.T, r *bufio.Reader, tag, want string) {
	t.Helper()
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			t.Fatalf("read %s reply: %v", tag, err)
		}
		if strings.HasPrefix(line, tag) {
			if !strings.HasPrefix(line, want) {
				t.Fatalf("got %q, want %s", strings.TrimSpace(line), want)
			}
			return
		}
	}
}

func fmtDeadlines(ds []time.Time, accept time.Time) string {
	parts := make([]string, len(ds))
	for i, d := range ds {
		if d.IsZero() {
			parts[i] = "cleared"
		} else {
			parts[i] = fmt.Sprintf("accept+%v", d.Sub(accept).Round(time.Millisecond))
		}
	}
	return "[" + strings.Join(parts, " ") + "]"
}
