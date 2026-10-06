// IMAP server skeleton tests — pre-AUTH greeting + CAPABILITY only.
// The production Backend / Session (backend.go, session.go) is what
// these tests exercise; recordingCaller in session_test.go satisfies
// the wsrpc.Caller dependency without touching the network.
package imap

import (
	"bufio"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"io"
	"log/slog"
	"math/big"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
)

// TestServerAcceptsTLSConnection verifies the server binds to a TLS
// listener, accepts a connection, and emits the standard `* OK ` IMAP
// greeting per RFC 9051 §7.1.1.
func TestServerAcceptsTLSConnection(t *testing.T) {
	cert := selfSignedCert(t)
	listener := newTLSListener(t, cert)
	defer listener.Close()

	backend := NewBackend(&recordingCaller{}, slog.Default(), 0, 0, 0, nil, nil)
	srv := NewServer(ServerConfig{}, backend)
	go func() { _ = srv.Serve(listener) }()
	defer func() { _ = srv.Close() }()

	conn, err := tls.Dial("tcp", listener.Addr().String(), &tls.Config{InsecureSkipVerify: true})
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer conn.Close()

	if err := conn.SetReadDeadline(time.Now().Add(2 * time.Second)); err != nil {
		t.Fatalf("SetReadDeadline: %v", err)
	}
	line, err := bufio.NewReader(conn).ReadString('\n')
	if err != nil {
		t.Fatalf("ReadString: %v", err)
	}
	if !strings.HasPrefix(line, "* OK ") {
		t.Fatalf("greeting = %q, want \"* OK ...\"", line)
	}
}

// TestServerAdvertisesIMAPCapabilities verifies the *pre-auth*
// CAPABILITY response names IMAP4rev2 (the baseline RFC 9051 dialect
// that folds in IDLE, UIDPLUS, ENABLE, LIST-EXTENDED, MOVE, etc.) plus
// the AUTH= mechanism for the current connection state, and that it
// does NOT name the post-auth-gated or upstream-blocked extensions.
// CONDSTORE (T3.2-a) + QRESYNC (T3.2-b) are wire-implemented in the
// FAUNA-FORK but advertised only post-auth (RFC 7162 ENABLE-gating);
// NOTIFY / QUOTA / SORT / THREAD remain upstream-blocked per
// imap-server.md § Upstream-blocked gaps. The fork's post-auth
// advertisement is covered by TestForkQresyncWireSeams /
// TestForkCondstoreWireSeams in third_party/go-imap.
func TestServerAdvertisesIMAPCapabilities(t *testing.T) {
	cert := selfSignedCert(t)
	listener := newTLSListener(t, cert)
	defer listener.Close()

	backend := NewBackend(&recordingCaller{}, slog.Default(), 0, 0, 0, nil, nil)
	srv := NewServer(ServerConfig{}, backend)
	go func() { _ = srv.Serve(listener) }()
	defer func() { _ = srv.Close() }()

	conn, err := tls.Dial("tcp", listener.Addr().String(), &tls.Config{InsecureSkipVerify: true})
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer conn.Close()
	conn.SetReadDeadline(time.Now().Add(2 * time.Second))

	r := bufio.NewReader(conn)
	// Read greeting.
	if _, err := r.ReadString('\n'); err != nil {
		t.Fatalf("read greeting: %v", err)
	}
	// Send CAPABILITY.
	if _, err := io.WriteString(conn, "A001 CAPABILITY\r\n"); err != nil {
		t.Fatalf("write CAPABILITY: %v", err)
	}
	// Read until A001 OK / NO / BAD.
	var capLine string
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			t.Fatalf("read response: %v", err)
		}
		if strings.HasPrefix(line, "* CAPABILITY ") {
			capLine = strings.TrimSpace(line)
		}
		if strings.HasPrefix(line, "A001 ") {
			break
		}
	}
	if capLine == "" {
		t.Fatal("server never emitted * CAPABILITY")
	}
	// Required pre-AUTH: IMAP4rev2 AND IMAP4rev1 + at least one AUTH
	// mechanism. rev1 is advertised alongside rev2 for backward
	// compatibility (RFC 9051 §2, imap-server.md § Capabilities) so
	// rev1-only clients (Python imaplib, older MUAs) can connect at all —
	// a rev2-only advertisement makes imaplib raise "server not IMAP4
	// compliant". IMAP4rev2 (RFC 9051 appendix E) still folds in IDLE,
	// ENABLE, UIDPLUS, LIST-EXTENDED, MOVE, etc. for rev2 clients.
	want := []imap.Cap{imap.CapIMAP4rev2, imap.CapIMAP4rev1}
	for _, c := range want {
		if !strings.Contains(capLine, string(c)) {
			t.Errorf("CAPABILITY missing %s — got %q", c, capLine)
		}
	}
	if !strings.Contains(capLine, "AUTH=") {
		t.Errorf("CAPABILITY missing AUTH=<mech> — got %q", capLine)
	}
	// Must NOT appear in the *pre-auth* CAPABILITY line. Two reasons,
	// per imap-server.md § Upstream-blocked gaps (advertising what we
	// can't generate is worse than silent non-implementation):
	//   - CONDSTORE (T3.2-a) + QRESYNC (T3.2-b) are wire-implemented but
	//     post-auth-only — RFC 7162 gates them behind ENABLE, so the
	//     fork advertises them only in the authenticated/selected state.
	//   - SORT / THREAD / MANAGESIEVE / NOTIFY / QUOTA are genuinely not
	//     generatable yet (no fork seam), so the library drops them.
	bad := []imap.Cap{imap.CapSort, "THREAD", "MANAGESIEVE", imap.CapCondStore, imap.CapQResync, imap.CapNotify, imap.CapQuota}
	for _, c := range bad {
		if strings.Contains(capLine, string(c)) {
			t.Errorf("CAPABILITY unexpectedly contains %s (upstream-blocked or post-auth-only) — got %q", c, capLine)
		}
	}
}

// ── helpers ────────────────────────────────────────────────────────

func selfSignedCert(t *testing.T) tls.Certificate {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatalf("GenerateKey: %v", err)
	}
	template := x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "fauna-mail-bridge-test"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		KeyUsage:     x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		DNSNames:     []string{"localhost"},
		IPAddresses:  []net.IP{net.IPv4(127, 0, 0, 1), net.IPv6loopback},
	}
	der, err := x509.CreateCertificate(rand.Reader, &template, &template, &key.PublicKey, key)
	if err != nil {
		t.Fatalf("CreateCertificate: %v", err)
	}
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key, Leaf: &template}
}

func newTLSListener(t *testing.T, cert tls.Certificate) net.Listener {
	t.Helper()
	ln, err := tls.Listen("tcp", "127.0.0.1:0", &tls.Config{Certificates: []tls.Certificate{cert}})
	if err != nil {
		t.Fatalf("tls.Listen: %v", err)
	}
	return ln
}
