package main

import (
	"bufio"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/binary"
	"fmt"
	"io"
	"log/slog"
	"math/big"
	"net"
	"net/http"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/proxyproto"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// encodeProxyV2 builds a minimal PROXY protocol v2 header (TCP over IPv4) —
// the byte layout `internal/proxyproto` peels and the SNI router emits
// (minus the router-auth TLV, which this listener skips by design).
func encodeProxyV2(src, dst *net.TCPAddr) []byte {
	sig := []byte{0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A}
	hdr := append([]byte{}, sig...)
	hdr = append(hdr, 0x21, 0x11) // v2 PROXY, TCP/IPv4
	hdr = binary.BigEndian.AppendUint16(hdr, 12)
	hdr = append(hdr, src.IP.To4()...)
	hdr = append(hdr, dst.IP.To4()...)
	hdr = binary.BigEndian.AppendUint16(hdr, uint16(src.Port))
	hdr = binary.BigEndian.AppendUint16(hdr, uint16(dst.Port))
	return hdr
}

// TestXrpcListenerStackKeysRateLimitOnProxyV2ClientIP isolates the proxyproto
// layer of the `--xrpc-listen` stack (proxyproto → http.Serve; TLS termination
// is added on top in production and locked by the ordering test below) and
// locks the property the packaging depends on: the per-IP rate limiter keys on
// the PROXY-v2-conveyed client IP, not the router's loopback source — and a
// direct headerless dial (the tier_3 path) still works untouched.
func TestXrpcListenerStackKeysRateLimitOnProxyV2ClientIP(t *testing.T) {
	srv := xrpc.NewServer(nil, nil, nil, xrpc.NewIPLimiter(nil), slog.New(slog.DiscardHandler))
	srv.Register(xrpc.Route{
		NSID:   "com.atproto.test.echo",
		Method: http.MethodGet,
		Auth:   xrpc.Public,
		Class:  xrpc.ClassAuth, // tightest window (10/5 min) — cheap to exhaust
		Handle: func(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
			w.WriteHeader(http.StatusOK)
		},
	})

	rawXrpc, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	httpServer := &http.Server{Handler: srv}
	go func() { _ = httpServer.Serve(proxyproto.New(rawXrpc)) }()
	t.Cleanup(func() { _ = httpServer.Close() })
	addr := rawXrpc.Addr().(*net.TCPAddr)

	// One raw HTTP GET, optionally preceded by a PROXY v2 header naming
	// `spoofSrc` as the real client.
	doGet := func(spoofSrc string) int {
		conn, derr := net.Dial("tcp", addr.String())
		if derr != nil {
			t.Fatalf("dial: %v", derr)
		}
		defer conn.Close()
		_ = conn.SetDeadline(time.Now().Add(10 * time.Second))
		if spoofSrc != "" {
			src := &net.TCPAddr{IP: net.ParseIP(spoofSrc), Port: 40000}
			if _, werr := conn.Write(encodeProxyV2(src, addr)); werr != nil {
				t.Fatalf("write proxy header: %v", werr)
			}
		}
		req := fmt.Sprintf("GET /xrpc/com.atproto.test.echo HTTP/1.1\r\nHost: pds.test\r\nConnection: close\r\n\r\n")
		if _, werr := io.WriteString(conn, req); werr != nil {
			t.Fatalf("write request: %v", werr)
		}
		resp, rerr := http.ReadResponse(bufio.NewReader(conn), nil)
		if rerr != nil {
			t.Fatalf("read response: %v", rerr)
		}
		defer resp.Body.Close()
		_, _ = io.Copy(io.Discard, resp.Body)
		return resp.StatusCode
	}

	// Exhaust ClassAuth (10/5 min) as conveyed client 198.51.100.7.
	for i := 0; i < 10; i++ {
		if got := doGet("198.51.100.7"); got != http.StatusOK {
			t.Fatalf("request %d as .7: want 200, got %d", i, got)
		}
	}
	if got := doGet("198.51.100.7"); got != http.StatusTooManyRequests {
		t.Fatalf("11th request as .7: want 429, got %d", got)
	}

	// A different conveyed client is a different bucket — if the limiter
	// were keying on the (shared loopback) transport source, this would be
	// 429 too and the whole internet would share one bucket.
	if got := doGet("198.51.100.8"); got != http.StatusOK {
		t.Fatalf("request as .8 after .7 exhausted: want 200, got %d", got)
	}

	// Headerless direct dial (the tier_3 loopback path) is untouched — the
	// loopback source is its own bucket, still open.
	if got := doGet(""); got != http.StatusOK {
		t.Fatalf("headerless direct dial: want 200, got %d", got)
	}
}

// selfSignedTLSConfig builds a throwaway ECDSA self-signed server cert for
// pds.test — stand-in for the sealed floor cert the bridgetls.Provider serves in
// production. Test-only; the real cert path (fetch → unseal → serve) is proven
// end-to-end by the tier_3 e2e (HTTPS skip-verify against the nest floor).
func selfSignedTLSConfig(t *testing.T) *tls.Config {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatalf("keygen: %v", err)
	}
	tmpl := &x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: "pds.test"},
		DNSNames:              []string{"pds.test"},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().Add(time.Hour),
		KeyUsage:              x509.KeyUsageDigitalSignature,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid: true,
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		t.Fatalf("cert: %v", err)
	}
	return &tls.Config{
		Certificates: []tls.Certificate{{Certificate: [][]byte{der}, PrivateKey: key}},
		MinVersion:   tls.VersionTLS12,
	}
}

// TestXrpcListenerTerminatesTLSOverProxyV2 locks the exact production listener
// stack main.go assembles: tls.NewListener(proxyproto.New(rawXrpc), cfg) —
// proxyproto INNERMOST. A client that sends [PROXY-v2 header][TLS ClientHello]
// [HTTP GET] must be served: the listener peels the PROXY header off the raw
// socket FIRST (before the TLS ClientHello arrives), then completes the TLS
// handshake on the real-client conn, then serves the request. If proxyproto sat
// OUTSIDE TLS the PROXY header bytes would be fed into the TLS parser and the
// handshake would fail — so this test is the headless guard on the ordering that
// neither the tier_3 headerless dial nor a deferred tier_4 exercises.
func TestXrpcListenerTerminatesTLSOverProxyV2(t *testing.T) {
	srv := xrpc.NewServer(nil, nil, nil, xrpc.NewIPLimiter(nil), slog.New(slog.DiscardHandler))
	srv.Register(xrpc.Route{
		NSID:   "com.atproto.test.echo",
		Method: http.MethodGet,
		Auth:   xrpc.Public,
		Class:  xrpc.ClassAuth,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *xrpc.Caller) {
			w.WriteHeader(http.StatusOK)
		},
	})

	rawXrpc, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	// The production stack (main.go): proxyproto INNERMOST, TLS outermost.
	ln := tls.NewListener(proxyproto.New(rawXrpc), selfSignedTLSConfig(t))
	httpServer := &http.Server{Handler: srv}
	go func() { _ = httpServer.Serve(ln) }()
	t.Cleanup(func() { _ = httpServer.Close() })
	addr := rawXrpc.Addr().(*net.TCPAddr)

	conn, derr := net.Dial("tcp", addr.String())
	if derr != nil {
		t.Fatalf("dial: %v", derr)
	}
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(10 * time.Second))

	// (1) PROXY-v2 header naming the real client — arrives on the raw socket.
	src := &net.TCPAddr{IP: net.ParseIP("198.51.100.9"), Port: 40000}
	if _, werr := conn.Write(encodeProxyV2(src, addr)); werr != nil {
		t.Fatalf("write proxy header: %v", werr)
	}
	// (2) TLS handshake over the SAME conn — succeeds only if proxyproto peeled
	//     the header first (else these bytes corrupt the TLS ClientHello parse).
	tconn := tls.Client(conn, &tls.Config{InsecureSkipVerify: true, ServerName: "pds.test"})
	if herr := tconn.Handshake(); herr != nil {
		t.Fatalf("tls handshake over proxyproto (ordering wrong?): %v", herr)
	}
	// (3) HTTP GET inside TLS — the request is served.
	req := "GET /xrpc/com.atproto.test.echo HTTP/1.1\r\nHost: pds.test\r\nConnection: close\r\n\r\n"
	if _, werr := io.WriteString(tconn, req); werr != nil {
		t.Fatalf("write request: %v", werr)
	}
	resp, rerr := http.ReadResponse(bufio.NewReader(tconn), nil)
	if rerr != nil {
		t.Fatalf("read response: %v", rerr)
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("TLS-over-PROXYv2 GET: want 200, got %d", resp.StatusCode)
	}
}
