// FAUNA-FORK: wire-level tests proving the additive QUOTA (RFC 9208)
// server-framework seams (see FORK.md row 17). These exercise the fork's
// GETQUOTA / GETQUOTAROOT command parsers, the SessionQuota dispatch
// hook, and the `* QUOTA` / `* QUOTAROOT` response writers end-to-end
// over a real connection — the MDA's seam-level tests (which call
// Session.GetQuota directly) cannot reach the command parser or the
// response writers. A minimal PreAuth Session stands in for the MDA so
// the test depends on no fauna crates. The shared `command` /
// `discardLogger` helpers live in condstore_fauna_test.go (same package).
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

// quotaSession is a minimal PreAuth Session implementing SessionQuota so
// the QUOTA fork seams are wire-observable.
type quotaSession struct {
	imapserver.SessionIMAP4rev2
}

func (quotaSession) Close() error                              { return nil }
func (quotaSession) Login(_, _ string) error                   { return imapserver.ErrAuthFailed }
func (quotaSession) Poll(*imapserver.UpdateWriter, bool) error { return nil }

func (quotaSession) GetQuota(root string) (*imapserver.QuotaData, error) {
	return &imapserver.QuotaData{
		Root: root,
		Resources: map[imap.QuotaResourceType]imapserver.QuotaResourceData{
			imap.QuotaResourceStorage: {Usage: 1, Limit: 1048576}, // KiB
			imap.QuotaResourceMessage: {Usage: 3, Limit: 50000},
		},
	}, nil
}

func (quotaSession) GetQuotaRoot(_ string) []string {
	return []string{"user/alice"}
}

// startQuotaServer spins a QUOTA-advertising PreAuth server on a loopback
// port and returns a connected raw client + reader.
func startQuotaServer(t *testing.T) (net.Conn, *bufio.Reader) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	srv := imapserver.New(&imapserver.Options{
		NewSession: func(*imapserver.Conn) (imapserver.Session, *imapserver.GreetingData, error) {
			return quotaSession{}, &imapserver.GreetingData{PreAuth: true}, nil
		},
		Caps:   imap.CapSet{imap.CapIMAP4rev2: {}, imap.CapQuota: {}},
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

func TestForkQuotaWireSeams(t *testing.T) {
	conn, r := startQuotaServer(t)

	// Greeting: PreAuth → authenticated state → QUOTA advertised with its
	// RFC 9208 §6 resource-type capabilities.
	greeting, err := r.ReadString('\n')
	if err != nil {
		t.Fatalf("read greeting: %v", err)
	}
	for _, want := range []string{"QUOTA", "QUOTA=RES-STORAGE", "QUOTA=RES-MESSAGE"} {
		if !strings.Contains(greeting, want) {
			t.Errorf("greeting must advertise %q post-auth, got %q", want, greeting)
		}
	}

	// GETQUOTAROOT INBOX → `* QUOTAROOT INBOX "user/alice"` followed by the
	// `* QUOTA "user/alice" (STORAGE 1 1048576 MESSAGE 3 50000)` for that
	// root. Proves the GETQUOTAROOT parser dispatched through SessionQuota
	// and both writeQuotaRoot + writeQuota emitted.
	rootResp := command(t, conn, r, "a1", "GETQUOTAROOT INBOX")
	if !strings.Contains(rootResp, "QUOTAROOT INBOX") || !strings.Contains(rootResp, "user/alice") {
		t.Errorf("GETQUOTAROOT: want `* QUOTAROOT INBOX user/alice`, got %q", rootResp)
	}
	if !strings.Contains(rootResp, "QUOTA") || !strings.Contains(rootResp, "STORAGE 1 1048576") {
		t.Errorf("GETQUOTAROOT: want inline `* QUOTA ... STORAGE 1 1048576`, got %q", rootResp)
	}
	if !strings.Contains(rootResp, "MESSAGE 3 50000") {
		t.Errorf("GETQUOTAROOT: want `MESSAGE 3 50000`, got %q", rootResp)
	}
	// Deterministic resource order: STORAGE must precede MESSAGE.
	if si, mi := strings.Index(rootResp, "STORAGE"), strings.Index(rootResp, "MESSAGE"); si == -1 || mi == -1 || si > mi {
		t.Errorf("GETQUOTAROOT: STORAGE must precede MESSAGE in the resource list, got %q", rootResp)
	}

	// GETQUOTA "user/alice" → just the `* QUOTA` line. Proves the GETQUOTA
	// command parser (ExpectAString root) + writeQuota emit.
	quotaResp := command(t, conn, r, "a2", `GETQUOTA "user/alice"`)
	if !strings.Contains(quotaResp, `QUOTA`) || !strings.Contains(quotaResp, "user/alice") {
		t.Errorf("GETQUOTA: want `* QUOTA user/alice (...)`, got %q", quotaResp)
	}
	if !strings.Contains(quotaResp, "STORAGE 1 1048576") || !strings.Contains(quotaResp, "MESSAGE 3 50000") {
		t.Errorf("GETQUOTA: want `(STORAGE 1 1048576 MESSAGE 3 50000)`, got %q", quotaResp)
	}
}
