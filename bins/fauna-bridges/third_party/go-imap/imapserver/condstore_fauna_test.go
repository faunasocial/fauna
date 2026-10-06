// FAUNA-FORK: wire-level tests proving the additive CONDSTORE
// server-framework seams (see FORK.md). These exercise the fork's
// parser + response-writer changes end-to-end over a real connection,
// which the MDA's seam-level tests (which bypass the wire decoder)
// cannot reach. A minimal PreAuth Session stands in for the MDA so the
// test depends on no fauna crates.
package imapserver_test

import (
	"bufio"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"
)

// condstoreSession is a minimal PreAuth Session. It embeds the
// SessionIMAP4rev2 interface (nil) so it satisfies the type assertions
// in Conn.serve without hand-stubbing ~25 methods; only the methods the
// CONDSTORE tests drive are overridden.
type condstoreSession struct {
	imapserver.SessionIMAP4rev2
}

func (condstoreSession) Close() error            { return nil }
func (condstoreSession) Login(_, _ string) error { return imapserver.ErrAuthFailed }
func (condstoreSession) Unselect() error         { return nil }

// Poll is invoked by the server after every command in the
// authenticated/selected state; the embedded nil interface would panic,
// so stub it.
func (condstoreSession) Poll(*imapserver.UpdateWriter, bool) error { return nil }

func (condstoreSession) Select(_ string, _ *imap.SelectOptions) (*imap.SelectData, error) {
	return &imap.SelectData{
		NumMessages:    1,
		UIDValidity:    1,
		UIDNext:        2,
		HighestModSeq:  7,
		Flags:          []imap.Flag{imap.FlagSeen},
		PermanentFlags: []imap.Flag{imap.FlagSeen},
	}, nil
}

func (condstoreSession) Fetch(w *imapserver.FetchWriter, _ imap.NumSet, options *imap.FetchOptions) error {
	mw := w.CreateMessage(1)
	mw.WriteUID(1)
	mw.WriteFlags([]imap.Flag{imap.FlagSeen})
	// FAUNA-FORK seam: emit MODSEQ only when the client requested it,
	// proving the FETCH (MODSEQ) parser populated options.ModSeq.
	if options != nil && options.ModSeq {
		mw.WriteModSeq(42)
	}
	return mw.Close()
}

func (condstoreSession) Store(w *imapserver.FetchWriter, _ imap.NumSet, _ *imap.StoreFlags, options *imap.StoreOptions) error {
	mw := w.CreateMessage(1)
	mw.WriteUID(1)
	mw.WriteFlags([]imap.Flag{imap.FlagSeen})
	// FAUNA-FORK seam: echo the parsed (UNCHANGEDSINCE n) back as MODSEQ
	// so the STORE-modifier parser is wire-observable (a value of n on
	// the wire proves options.UnchangedSince was populated from it).
	mw.WriteModSeq(options.UnchangedSince)
	return mw.Close()
}

// startCondstoreServer spins a CONDSTORE-advertising PreAuth server on a
// loopback port and returns a connected raw client + reader.
func startCondstoreServer(t *testing.T) (net.Conn, *bufio.Reader) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	srv := imapserver.New(&imapserver.Options{
		NewSession: func(*imapserver.Conn) (imapserver.Session, *imapserver.GreetingData, error) {
			return condstoreSession{}, &imapserver.GreetingData{PreAuth: true}, nil
		},
		Caps: imap.CapSet{imap.CapIMAP4rev2: {}, imap.CapCondStore: {}},
		// log to t-discard so a deliberately-failing handler doesn't spam.
		Logger: discardLogger{},
	})
	go func() { _ = srv.Serve(ln) }()
	t.Cleanup(func() { _ = srv.Close(); _ = ln.Close() })

	conn, err := net.Dial("tcp", ln.Addr().String())
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	_ = conn.SetDeadline(time.Now().Add(5 * time.Second))
	return conn, bufio.NewReader(conn)
}

type discardLogger struct{}

func (discardLogger) Printf(string, ...interface{}) {}

// command sends a tagged command and returns the full response block up
// to and including the tagged status line.
func command(t *testing.T, conn net.Conn, r *bufio.Reader, tag, cmd string) string {
	t.Helper()
	if _, err := conn.Write([]byte(tag + " " + cmd + "\r\n")); err != nil {
		t.Fatalf("write %s: %v", cmd, err)
	}
	var b strings.Builder
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			t.Fatalf("read after %s: %v (got so far: %q)", cmd, err, b.String())
		}
		b.WriteString(line)
		if strings.HasPrefix(line, tag+" ") {
			return b.String()
		}
	}
}

func TestForkCondstoreWireSeams(t *testing.T) {
	conn, r := startCondstoreServer(t)

	// Greeting: PreAuth → authenticated state → CONDSTORE advertised.
	greeting, err := r.ReadString('\n')
	if err != nil {
		t.Fatalf("read greeting: %v", err)
	}
	if !strings.Contains(greeting, "CONDSTORE") {
		t.Errorf("greeting must advertise CONDSTORE post-auth, got %q", greeting)
	}

	// ENABLE CONDSTORE → `* ENABLED CONDSTORE`.
	if resp := command(t, conn, r, "a1", "ENABLE CONDSTORE"); !strings.Contains(resp, "ENABLED CONDSTORE") {
		t.Errorf("ENABLE CONDSTORE: want `* ENABLED CONDSTORE`, got %q", resp)
	}

	// SELECT after CONDSTORE enabled → OK [HIGHESTMODSEQ 7].
	if resp := command(t, conn, r, "a2", "SELECT INBOX"); !strings.Contains(resp, "HIGHESTMODSEQ 7") {
		t.Errorf("SELECT: want HIGHESTMODSEQ 7, got %q", resp)
	}

	// FETCH (MODSEQ) → MODSEQ (42). Proves the MODSEQ fetch-item parser
	// populated options.ModSeq AND FetchResponseWriter.WriteModSeq emits.
	if resp := command(t, conn, r, "a3", "FETCH 1 (FLAGS MODSEQ)"); !strings.Contains(resp, "MODSEQ (42)") {
		t.Errorf("FETCH (MODSEQ): want MODSEQ (42), got %q", resp)
	}

	// STORE (UNCHANGEDSINCE 5) → the session echoes the parsed value as
	// MODSEQ; MODSEQ (5) proves the store-modifier parser populated
	// options.UnchangedSince.
	if resp := command(t, conn, r, "a4", `STORE 1 (UNCHANGEDSINCE 5) +FLAGS (\Seen)`); !strings.Contains(resp, "MODSEQ (5)") {
		t.Errorf("STORE (UNCHANGEDSINCE 5): want echoed MODSEQ (5), got %q", resp)
	}
}
