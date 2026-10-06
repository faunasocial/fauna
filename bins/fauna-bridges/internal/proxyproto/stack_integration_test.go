package proxyproto

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"fmt"
	"math/big"
	"net"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/connlimit"
)

func selfSignedCertForTest(t *testing.T) tls.Certificate {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatalf("GenerateKey: %v", err)
	}
	template := x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "fauna-proxyproto-test"},
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

// TestStackPeelsHeaderBelowTLSAndConnlimit replicates the EXACT mda.go CalDAV-443
// listener stack — tls.NewListener(connlimit.New(proxyproto.New(raw))) — and
// drives a real TLS handshake preceded by a PROXY-v2 header. It proves the two
// subtle invariants the deploy depends on:
//
//  1. The header is peeled on the raw socket BELOW both wrappers, so the TLS
//     ClientHello that follows it handshakes cleanly, and the accepted conn
//     still type-asserts to *tls.Conn (so http.Server populates r.TLS).
//  2. The conveyed real client IP surfaces all the way up through the connlimit
//     slotConn and the *tls.Conn to RemoteAddr() — the seam CalDAV's clientIP(r)
//     reads.
func TestStackPeelsHeaderBelowTLSAndConnlimit(t *testing.T) {
	cert := selfSignedCertForTest(t)
	tlsConf := &tls.Config{Certificates: []tls.Certificate{cert}}

	raw, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer func() { _ = raw.Close() }()
	// proxyproto INNERMOST, then connlimit, then TLS — byte-for-byte the mda.go
	// wrap order.
	ln := tls.NewListener(connlimit.New(New(raw), 4096, nil, nil), tlsConf)

	external := &net.TCPAddr{IP: net.ParseIP("203.0.113.7"), Port: 54321}

	type result struct {
		addr     string
		isTLS    bool
		handshOK bool
		err      error
	}
	res := make(chan result, 1)
	go func() {
		c, err := ln.Accept()
		if err != nil {
			res <- result{err: fmt.Errorf("accept: %w", err)}
			return
		}
		defer func() { _ = c.Close() }()
		tc, ok := c.(*tls.Conn)
		if !ok {
			res <- result{err: fmt.Errorf("accepted conn is %T, want *tls.Conn", c)}
			return
		}
		hsErr := tc.Handshake()
		res <- result{
			addr:     c.RemoteAddr().String(),
			isTLS:    true,
			handshOK: hsErr == nil,
			err:      hsErr,
		}
	}()

	rawConn, err := net.Dial("tcp", raw.Addr().String())
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	// Write the PROXY-v2 header on the raw socket, THEN do TLS over the same
	// conn — exactly how the SNI router prepends the header before splicing the
	// client's ClientHello.
	header := encodeV2ForTest(external, &net.TCPAddr{IP: net.ParseIP("198.51.100.1"), Port: 443})
	if _, err := rawConn.Write(header); err != nil {
		t.Fatalf("write proxy header: %v", err)
	}
	client := tls.Client(rawConn, &tls.Config{InsecureSkipVerify: true}) //nolint:gosec // self-signed test cert
	if err := client.Handshake(); err != nil {
		t.Fatalf("client handshake: %v", err)
	}
	defer func() { _ = client.Close() }()

	r := <-res
	if r.err != nil {
		t.Fatalf("server: %v", r.err)
	}
	if !r.isTLS {
		t.Fatalf("accepted conn did not type-assert to *tls.Conn")
	}
	if !r.handshOK {
		t.Fatalf("server handshake failed (header not peeled cleanly below TLS)")
	}
	if r.addr != external.String() {
		t.Fatalf("server RemoteAddr = %v, want conveyed external %v (real IP did not surface through connlimit+tls)", r.addr, external)
	}
}

// TestStackAuthedHeaderVerifiesAndPreservesTLS runs the EXACT mda.go CalDAV-443
// stack with a router-auth secret provisioned and proves the two halves
// the deploy depends on, over a real TLS handshake:
//
//  1. A correctly-authed header from the router still conveys the real client IP
//     all the way to RemoteAddr() — the TLV rides AFTER the address block and
//     lengthens the header the per-IP cap's early peel consumes, so a mis-sized
//     Discard would corrupt the ClientHello and fail the handshake here.
//  2. An unauthenticated header from a co-resident forger on the same loopback
//     is refused — RemoteAddr() stays the genuine loopback peer — and the
//     connection still handshakes, so the forger cannot even tell it was caught.
func TestStackAuthedHeaderVerifiesAndPreservesTLS(t *testing.T) {
	secret := []byte("a-32-byte-ish-router-auth-secret")
	external := &net.TCPAddr{IP: net.ParseIP("203.0.113.7"), Port: 54321}
	dst := &net.TCPAddr{IP: net.ParseIP("198.51.100.1"), Port: 443}

	for _, tc := range []struct {
		name       string
		tlv        []byte // the secret the client's header carries (nil = none)
		wantExtern bool
	}{
		{"router header with the matching TLV is trusted", secret, true},
		{"co-resident forgery with no TLV is refused", nil, false},
		{"co-resident forgery with a wrong TLV is refused", []byte("wrong-secret"), false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cert := selfSignedCertForTest(t)
			tlsConf := &tls.Config{Certificates: []tls.Certificate{cert}}

			raw, err := net.Listen("tcp", "127.0.0.1:0")
			if err != nil {
				t.Fatalf("listen: %v", err)
			}
			defer func() { _ = raw.Close() }()
			limiter := connlimit.NewPerIPLimiter(256)
			ln := tls.NewListener(
				connlimit.New(connlimit.NewPerIPListener(New(raw, WithRouterAuth(secret)), limiter, nil), 4096, nil, nil),
				tlsConf)

			type result struct {
				addr     string
				handshOK bool
				err      error
			}
			res := make(chan result, 1)
			go func() {
				c, err := ln.Accept()
				if err != nil {
					res <- result{err: fmt.Errorf("accept: %w", err)}
					return
				}
				defer func() { _ = c.Close() }()
				tc, ok := c.(*tls.Conn)
				if !ok {
					res <- result{err: fmt.Errorf("accepted conn is %T, want *tls.Conn", c)}
					return
				}
				hsErr := tc.Handshake()
				res <- result{addr: c.RemoteAddr().String(), handshOK: hsErr == nil, err: hsErr}
			}()

			rawConn, err := net.Dial("tcp", raw.Addr().String())
			if err != nil {
				t.Fatalf("dial: %v", err)
			}
			if _, err := rawConn.Write(encodeV2AuthedForTest(external, dst, tc.tlv)); err != nil {
				t.Fatalf("write proxy header: %v", err)
			}
			client := tls.Client(rawConn, &tls.Config{InsecureSkipVerify: true}) //nolint:gosec // self-signed test cert
			if err := client.Handshake(); err != nil {
				t.Fatalf("client handshake: %v", err)
			}
			defer func() { _ = client.Close() }()

			r := <-res
			if r.err != nil {
				t.Fatalf("server: %v", r.err)
			}
			if !r.handshOK {
				t.Fatalf("server handshake failed — the header (auth TLV included) was not consumed cleanly below TLS")
			}
			if tc.wantExtern {
				if r.addr != external.String() {
					t.Fatalf("server RemoteAddr = %v, want conveyed external %v", r.addr, external)
				}
				return
			}
			if r.addr == external.String() {
				t.Fatalf("server RemoteAddr = %v — an unauthenticated header spoofed the source through the full stack", r.addr)
			}
			host, _, err := net.SplitHostPort(r.addr)
			if err != nil {
				t.Fatalf("split: %v", err)
			}
			if !net.ParseIP(host).IsLoopback() {
				t.Fatalf("server RemoteAddr host = %q, want the genuine loopback peer", host)
			}
		})
	}
}

// TestStackPerIPCapPeelsHeaderAtAcceptAndPreservesTLS replicates the EXACT
// mda.go CalDAV-443 stack INCLUDING the per-IP cap that B(b.2) adds —
// tls.NewListener(connlimit.New(connlimit.NewPerIPListener(proxyproto.New(raw)))).
// The per-IP cap reads conn.RemoteAddr() at accept time to key on the source IP,
// which triggers proxyproto's lazy peel BEFORE the TLS handshake. This proves
// the new invariant that early peel introduces: the post-header bytes the cap's
// RemoteAddr() leaves buffered are exactly the TLS ClientHello, so the handshake
// still completes cleanly and the conveyed real IP surfaces through the per-IP
// conn, the connlimit slotConn, and the *tls.Conn to RemoteAddr().
func TestStackPerIPCapPeelsHeaderAtAcceptAndPreservesTLS(t *testing.T) {
	cert := selfSignedCertForTest(t)
	tlsConf := &tls.Config{Certificates: []tls.Certificate{cert}}

	raw, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer func() { _ = raw.Close() }()
	// proxyproto INNERMOST, then per-IP cap, then global cap, then TLS —
	// byte-for-byte the mda.go CalDAV wrap order (perIPMDA below capListener,
	// above proxyproto).
	limiter := connlimit.NewPerIPLimiter(256)
	ln := tls.NewListener(
		connlimit.New(connlimit.NewPerIPListener(New(raw), limiter, nil), 4096, nil, nil),
		tlsConf)

	external := &net.TCPAddr{IP: net.ParseIP("203.0.113.7"), Port: 54321}

	type result struct {
		addr     string
		isTLS    bool
		handshOK bool
		err      error
	}
	res := make(chan result, 1)
	go func() {
		c, err := ln.Accept()
		if err != nil {
			res <- result{err: fmt.Errorf("accept: %w", err)}
			return
		}
		defer func() { _ = c.Close() }()
		tc, ok := c.(*tls.Conn)
		if !ok {
			res <- result{err: fmt.Errorf("accepted conn is %T, want *tls.Conn", c)}
			return
		}
		hsErr := tc.Handshake()
		res <- result{addr: c.RemoteAddr().String(), isTLS: true, handshOK: hsErr == nil, err: hsErr}
	}()

	rawConn, err := net.Dial("tcp", raw.Addr().String())
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	header := encodeV2ForTest(external, &net.TCPAddr{IP: net.ParseIP("198.51.100.1"), Port: 443})
	if _, err := rawConn.Write(header); err != nil {
		t.Fatalf("write proxy header: %v", err)
	}
	client := tls.Client(rawConn, &tls.Config{InsecureSkipVerify: true}) //nolint:gosec // self-signed test cert
	if err := client.Handshake(); err != nil {
		t.Fatalf("client handshake: %v", err)
	}
	defer func() { _ = client.Close() }()

	r := <-res
	if r.err != nil {
		t.Fatalf("server: %v", r.err)
	}
	if !r.isTLS {
		t.Fatalf("accepted conn did not type-assert to *tls.Conn (per-IP wrapper broke TLS detection)")
	}
	if !r.handshOK {
		t.Fatalf("server handshake failed — the per-IP cap's early RemoteAddr() peel corrupted the ClientHello stream")
	}
	if r.addr != external.String() {
		t.Fatalf("server RemoteAddr = %v, want conveyed external %v (real IP did not surface through per-IP+connlimit+tls)", r.addr, external)
	}
}
