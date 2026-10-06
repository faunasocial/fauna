// FAUNA-FORK: wire-level tests proving the additive QRESYNC (RFC 7162
// §3.2) server-framework seams (see FORK.md). These exercise the fork's
// SELECT (QRESYNC ...) parameter parser, the (CHANGEDSINCE n VANISHED)
// FETCH-modifier parser, and the `* VANISHED` / `* VANISHED (EARLIER)`
// response writers end-to-end over a real connection — the MDA's
// seam-level tests (which bypass the wire decoder) cannot reach them.
// A minimal PreAuth Session stands in for the MDA so the test depends on
// no fauna crates. The shared `command` / `discardLogger` helpers live in
// condstore_fauna_test.go (same package).
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

// qresyncSession is a minimal PreAuth Session whose overridden methods
// make the QRESYNC fork seams wire-observable.
type qresyncSession struct {
	imapserver.SessionIMAP4rev2
}

func (qresyncSession) Close() error                              { return nil }
func (qresyncSession) Login(_, _ string) error                   { return imapserver.ErrAuthFailed }
func (qresyncSession) Unselect() error                           { return nil }
func (qresyncSession) Poll(*imapserver.UpdateWriter, bool) error { return nil }

func (qresyncSession) Select(_ string, options *imap.SelectOptions) (*imap.SelectData, error) {
	data := &imap.SelectData{
		NumMessages:    1,
		UIDValidity:    1,
		UIDNext:        2,
		Flags:          []imap.Flag{imap.FlagSeen},
		PermanentFlags: []imap.Flag{imap.FlagSeen},
	}
	// FAUNA-FORK seam: echo the parsed (QRESYNC (uidvalidity modseq))
	// modseq back as HIGHESTMODSEQ, proving the SELECT-param parser
	// populated options.QResync.ModSeq off the wire. (HIGHESTMODSEQ is
	// only emitted because ENABLE QRESYNC implies CONDSTORE — verifying
	// the CapSet implication routes through too.)
	if options != nil && options.QResync != nil {
		data.HighestModSeq = options.QResync.ModSeq
		// FAUNA-FORK seam: inline SELECT (QRESYNC ...) fast-path output
		// (RFC 7162 §3.2.5). Populating SelectData.QResync makes
		// handleSelect emit `* VANISHED (EARLIER) <set>` followed by the
		// changed-message `* <seq> FETCH (UID FLAGS MODSEQ)` responses
		// within the SELECT response itself — the common-case fast-path.
		data.QResync = &imap.SelectQResyncData{
			Vanished: imap.UIDSetNum(7, 9),
			Changed: []imap.SelectQResyncChange{
				{SeqNum: 1, UID: 1, Flags: []imap.Flag{imap.FlagSeen}, ModSeq: 6},
			},
		}
	}
	return data, nil
}

func (qresyncSession) Fetch(w *imapserver.FetchWriter, _ imap.NumSet, options *imap.FetchOptions) error {
	// FAUNA-FORK seam: when the (CHANGEDSINCE n VANISHED) FETCH modifier
	// parsed, emit `* VANISHED (EARLIER)` echoing ChangedSince as the
	// vanished UID — proves both options.Vanished and options.ChangedSince
	// reached the session AND FetchWriter.WriteVanishedEarlier emits.
	if options != nil && options.Vanished {
		if err := w.WriteVanishedEarlier(imap.UIDSetNum(imap.UID(options.ChangedSince))); err != nil {
			return err
		}
	}
	mw := w.CreateMessage(1)
	mw.WriteUID(1)
	mw.WriteFlags([]imap.Flag{imap.FlagSeen})
	return mw.Close()
}

func (qresyncSession) Expunge(w *imapserver.ExpungeWriter, _ *imap.UIDSet) error {
	// FAUNA-FORK seam: emit `* VANISHED <set>` (no EARLIER tag — a live
	// expunge) via the new ExpungeWriter.WriteVanished.
	return w.WriteVanished(imap.UIDSetNum(2, 4, 6))
}

// startQresyncServer spins a QRESYNC-advertising PreAuth server on a
// loopback port and returns a connected raw client + reader.
func startQresyncServer(t *testing.T) (net.Conn, *bufio.Reader) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	srv := imapserver.New(&imapserver.Options{
		NewSession: func(*imapserver.Conn) (imapserver.Session, *imapserver.GreetingData, error) {
			return qresyncSession{}, &imapserver.GreetingData{PreAuth: true}, nil
		},
		Caps:   imap.CapSet{imap.CapIMAP4rev2: {}, imap.CapQResync: {}},
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

func TestForkQresyncWireSeams(t *testing.T) {
	conn, r := startQresyncServer(t)

	// Greeting: PreAuth → authenticated state → QRESYNC advertised.
	greeting, err := r.ReadString('\n')
	if err != nil {
		t.Fatalf("read greeting: %v", err)
	}
	if !strings.Contains(greeting, "QRESYNC") {
		t.Errorf("greeting must advertise QRESYNC post-auth, got %q", greeting)
	}

	// ENABLE QRESYNC → `* ENABLED QRESYNC`.
	if resp := command(t, conn, r, "a1", "ENABLE QRESYNC"); !strings.Contains(resp, "ENABLED QRESYNC") {
		t.Errorf("ENABLE QRESYNC: want `* ENABLED QRESYNC`, got %q", resp)
	}

	// SELECT INBOX (QRESYNC (1 5)) → HIGHESTMODSEQ 5 (echoed) proves the
	// (QRESYNC (uidvalidity modseq)) SELECT-param parser populated
	// options.QResync.ModSeq AND that ENABLE QRESYNC implied CONDSTORE.
	// The inline fast-path (RFC 7162 §3.2.5) then emits `* VANISHED
	// (EARLIER) 7,9` and the changed-message `* 1 FETCH (... MODSEQ (6))`
	// within the SELECT response — proving SelectData.QResync drives
	// handleSelect to write both before the tagged OK.
	selectResp := command(t, conn, r, "a2", "SELECT INBOX (QRESYNC (1 5))")
	if !strings.Contains(selectResp, "HIGHESTMODSEQ 5") {
		t.Errorf("SELECT (QRESYNC (1 5)): want echoed HIGHESTMODSEQ 5, got %q", selectResp)
	}
	if !strings.Contains(selectResp, "VANISHED (EARLIER) 7,9") {
		t.Errorf("SELECT (QRESYNC ...): want inline `* VANISHED (EARLIER) 7,9`, got %q", selectResp)
	}
	if !strings.Contains(selectResp, "FETCH (UID 1 FLAGS (\\Seen) MODSEQ (6))") {
		t.Errorf("SELECT (QRESYNC ...): want inline changed-message FETCH, got %q", selectResp)
	}

	// UID FETCH 1 (FLAGS) (CHANGEDSINCE 3 VANISHED) → `* VANISHED (EARLIER) 3`.
	// Proves the VANISHED FETCH-modifier parser set options.Vanished, that
	// CHANGEDSINCE rode along, and FetchWriter.WriteVanishedEarlier emits.
	if resp := command(t, conn, r, "a3", "UID FETCH 1 (FLAGS) (CHANGEDSINCE 3 VANISHED)"); !strings.Contains(resp, "VANISHED (EARLIER) 3") {
		t.Errorf("UID FETCH (CHANGEDSINCE 3 VANISHED): want `* VANISHED (EARLIER) 3`, got %q", resp)
	}

	// EXPUNGE → `* VANISHED 2,4,6` (no EARLIER tag). Proves
	// ExpungeWriter.WriteVanished emits the untagged live-expunge form.
	if resp := command(t, conn, r, "a4", "EXPUNGE"); !strings.Contains(resp, "VANISHED 2,4,6") {
		t.Errorf("EXPUNGE: want `* VANISHED 2,4,6`, got %q", resp)
	}
}
