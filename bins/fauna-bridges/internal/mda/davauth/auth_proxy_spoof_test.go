package davauth

import (
	"bufio"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/binary"
	"log/slog"
	"math/big"
	"net"
	"net/http"
	"sync/atomic"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/auth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/connlimit"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/proxyproto"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// encodeProxyV2ForTest builds a PROXY-protocol-v2 PROXY-command header for a
// TCP/IPv4 connection from src to dst, optionally appending the router-auth
// TLV. Byte layout authority: internal/proxyproto's TestGoldenV4 /
// TestGoldenAuthedV4, which pin these exact bytes against the Rust router's
// fauna_proxy_protocol::encode_v2{,_authed}. A nil secret is what a co-resident
// forger — who cannot read the root-owned secret file — can produce.
func encodeProxyV2ForTest(src, dst *net.TCPAddr, secret []byte) []byte {
	const (
		verCmdProxy = 0x21 // v2 + PROXY
		famTCP4     = 0x11 // AF_INET + STREAM
		blockLen    = 12   // src(4) + dst(4) + sport(2) + dport(2)
	)
	payloadLen := blockLen
	if secret != nil {
		payloadLen += 3 + len(secret) // type(1) + length(2) + value
	}
	h := append([]byte{}, proxyproto.Signature[:]...)
	h = append(h, verCmdProxy, famTCP4)
	h = binary.BigEndian.AppendUint16(h, uint16(payloadLen))
	h = append(h, src.IP.To4()...)
	h = append(h, dst.IP.To4()...)
	h = binary.BigEndian.AppendUint16(h, uint16(src.Port))
	h = binary.BigEndian.AppendUint16(h, uint16(dst.Port))
	if secret != nil {
		h = append(h, 0xE0) // TLV_TYPE_ROUTER_AUTH
		h = binary.BigEndian.AppendUint16(h, uint16(len(secret)))
		h = append(h, secret...)
	}
	return h
}

func selfSignedCertForSpoofTest(t *testing.T) tls.Certificate {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatalf("GenerateKey: %v", err)
	}
	tmpl := x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "fauna-davauth-fc1-test"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		KeyUsage:     x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		DNSNames:     []string{"localhost"},
		IPAddresses:  []net.IP{net.IPv4(127, 0, 0, 1), net.IPv6loopback},
	}
	der, err := x509.CreateCertificate(rand.Reader, &tmpl, &tmpl, &key.PublicKey, key)
	if err != nil {
		t.Fatalf("CreateCertificate: %v", err)
	}
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key, Leaf: &tmpl}
}

// TestForgedProxyHeaderCannotPoisonTheAuthLockout is the end-to-end witness that
// the router-auth check actually protects the harm `security.md` § Co-resident process trust
// boundary names — "the MDA AUTH-lockout key" — rather than only nest's own
// accept path.
//
// It stands up the REAL dav-443 stack from mda.go — proxyproto (with the
// artifact-provisioned router-auth secret) → per-IP cap → global cap → TLS — in
// front of the REAL davauth middleware and its real authlock.Lockout, then plays
// the actual attacker: a loopback peer (a compromised co-resident bridge UID)
// that dials the loopback DAV port and prepends a well-formed PROXY-v2 header
// claiming a victim's egress IP, with no router-auth TLV because it cannot read
// the root-owned secret.
//
// Three assertions, one per harm in the review (H1 cross-user lockout DoS, H2
// spray-brake evasion, H3 audit misattribution):
//
//  1. the forged IP accrues NOTHING — IsLockedFor(…, forgedIP) stays false, so a
//     victim's real connections from that IP are never locked out;
//  2. the failures accrue against the attacker's OWN genuine loopback peer, so
//     the coarse-tier brake still bites and rotating forged IPs buys no evasion;
//  3. every report_auth_event carries the genuine loopback IP, so on-box attack
//     activity cannot be laundered through an innocent third-party address.
func TestForgedProxyHeaderCannotPoisonTheAuthLockout(t *testing.T) {
	const limit = 3
	secret := []byte("a-32-byte-ish-router-auth-secret")
	victimIP := "203.0.113.7"
	forged := &net.TCPAddr{IP: net.ParseIP(victimIP), Port: 54321}
	dst := &net.TCPAddr{IP: net.ParseIP("198.51.100.1"), Port: 443}

	lockout := &atomic.Pointer[authlock.Lockout]{}
	lockout.Store(authlock.New(limit, time.Minute, nil))
	caller := resolvableCaller(t)

	raw, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer func() { _ = raw.Close() }()

	cert := selfSignedCertForSpoofTest(t)
	tlsConf := &tls.Config{Certificates: []tls.Certificate{cert}}
	// Byte-for-byte the mda.go dav-443 wrap order.
	limiter := connlimit.NewPerIPLimiter(256)
	ln := tls.NewListener(
		connlimit.New(
			connlimit.NewPerIPListener(
				proxyproto.New(raw, proxyproto.WithRouterAuth(secret)), limiter, nil),
			4096, nil, nil),
		tlsConf)

	srv := &http.Server{
		Handler:           NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), lockout, nil),
		ReadHeaderTimeout: 10 * time.Second,
	}
	defer func() { _ = srv.Close() }()
	go func() { _ = srv.Serve(ln) }()

	// The attack: `limit` failed AUTHs, each on a fresh connection carrying a
	// forged header. Every one is a real TLS + HTTP request through the real
	// middleware.
	for i := 0; i < limit; i++ {
		conn, err := net.Dial("tcp", raw.Addr().String())
		if err != nil {
			t.Fatalf("attempt %d: dial: %v", i+1, err)
		}
		if _, err := conn.Write(encodeProxyV2ForTest(forged, dst, nil)); err != nil {
			t.Fatalf("attempt %d: write forged header: %v", i+1, err)
		}
		tc := tls.Client(conn, &tls.Config{InsecureSkipVerify: true}) //nolint:gosec // self-signed test cert
		req, err := http.NewRequest("PROPFIND",
			"https://127.0.0.1/caldav/"+fixtureLocalPart+"@"+fixtureDomain+"/", nil)
		if err != nil {
			t.Fatalf("attempt %d: new request: %v", i+1, err)
		}
		req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, "WRONG-PASSWORD")
		if err := req.Write(tc); err != nil {
			t.Fatalf("attempt %d: write request: %v", i+1, err)
		}
		resp, err := http.ReadResponse(bufio.NewReader(tc), req)
		if err != nil {
			t.Fatalf("attempt %d: read response: %v", i+1, err)
		}
		if resp.StatusCode != http.StatusUnauthorized {
			t.Fatalf("attempt %d: wrong password must 401, got %d", i+1, resp.StatusCode)
		}
		_ = resp.Body.Close()
		_ = tc.Close()
	}

	principal := auth.PrincipalKey(fixtureLocalPart, fixtureDomain)
	lo := lockout.Load()

	// (1) H1 — the victim's egress IP accrued nothing, so their real CalDAV
	// clients are not locked out. This is the assertion that fails without the
	// TLV check: the forged source would have carried all `limit` failures.
	if lo.IsLockedFor(principal, auth.DefaultCredentialID, victimIP) {
		t.Fatalf("forged source IP %s reached the lockout key — a co-resident forger can lock out every real user behind that address", victimIP)
	}
	// (2) H2 — the failures landed on the forger's own genuine loopback peer, so
	// the brake still bites and rotating forged IPs buys nothing.
	if !lo.IsLockedFor(principal, auth.DefaultCredentialID, "127.0.0.1") {
		t.Fatalf("after %d forged-header failures the genuine loopback peer is NOT locked out — the failures were attributed somewhere else entirely", limit)
	}
	// (3) H3 — the audit records the real origin, not the innocent third party.
	reports := caller.callsOf(wsrpc.MethodReportAuthEvent)
	if len(reports) != limit {
		t.Fatalf("report_auth_event fired %d times, want %d (one per failed AUTH)", len(reports), limit)
	}
	for i, rec := range reports {
		var m map[string]any
		if err := cbor.Unmarshal(rec.body, &m); err != nil {
			t.Fatalf("report %d: decode body: %v", i, err)
		}
		ip, _ := m["source_ip"].(string)
		if ip == victimIP {
			t.Fatalf("report %d stamped the forged source_ip %q — on-box attack activity is laundered through an innocent address", i, victimIP)
		}
		if !net.ParseIP(ip).IsLoopback() {
			t.Fatalf("report %d source_ip = %q, want the genuine loopback peer", i, ip)
		}
	}
}
