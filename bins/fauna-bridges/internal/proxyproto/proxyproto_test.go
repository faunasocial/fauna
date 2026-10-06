package proxyproto

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"io"
	"net"
	"testing"
	"time"
)

// encodeV2ForTest builds a PROXY-protocol-v2 PROXY-command header for a TCP
// connection from src to dst, byte-for-byte the way the Rust router's
// fauna_proxy_protocol::encode_v2 emits it. Test-only: the Go MDA only parses
// (the router encodes), so this mirrors the wire format we must stay
// compatible with.
func encodeV2ForTest(src, dst *net.TCPAddr) []byte {
	var h []byte
	h = append(h, Signature[:]...)
	h = append(h, verCmdProxy)
	v4 := src.IP.To4() != nil
	if v4 {
		h = append(h, famTCP4)
		h = binary.BigEndian.AppendUint16(h, tcp4BlockLen)
		h = append(h, src.IP.To4()...)
		h = append(h, dst.IP.To4()...)
		h = binary.BigEndian.AppendUint16(h, uint16(src.Port))
		h = binary.BigEndian.AppendUint16(h, uint16(dst.Port))
	} else {
		h = append(h, famTCP6)
		h = binary.BigEndian.AppendUint16(h, tcp6BlockLen)
		h = append(h, src.IP.To16()...)
		h = append(h, dst.IP.To16()...)
		h = binary.BigEndian.AppendUint16(h, uint16(src.Port))
		h = binary.BigEndian.AppendUint16(h, uint16(dst.Port))
	}
	return h
}

// encodeV2AuthedForTest builds the same header as encodeV2ForTest but appends a
// router-auth TLV (`type(1)=0xE0 || len(2 BE) || value`) after the address
// block, counted in the header's length field — byte-for-byte the way the Rust
// router's fauna_proxy_protocol::encode_v2_authed emits it. Passing a nil
// secret yields a plain header.
func encodeV2AuthedForTest(src, dst *net.TCPAddr, secret []byte) []byte {
	h := encodeV2ForTest(src, dst)
	if secret == nil {
		return h
	}
	// Grow the declared payload length by the TLV, then append it.
	binary.BigEndian.PutUint16(h[14:16],
		binary.BigEndian.Uint16(h[14:16])+uint16(tlvHeaderLen+len(secret)))
	h = append(h, tlvTypeRouterAuth)
	h = binary.BigEndian.AppendUint16(h, uint16(len(secret)))
	return append(h, secret...)
}

// fakeConn is a net.Conn with a settable RemoteAddr and a canned read buffer,
// so the loopback-trust gate can be exercised against a non-loopback peer
// (which a real ephemeral 127.0.0.1 listener pair cannot produce).
type fakeConn struct {
	r      *bytes.Reader
	remote net.Addr
}

func (f *fakeConn) Read(p []byte) (int, error)         { return f.r.Read(p) }
func (f *fakeConn) Write(p []byte) (int, error)        { return len(p), nil }
func (f *fakeConn) Close() error                       { return nil }
func (f *fakeConn) LocalAddr() net.Addr                { return f.remote }
func (f *fakeConn) RemoteAddr() net.Addr               { return f.remote }
func (f *fakeConn) SetDeadline(_ time.Time) error      { return nil }
func (f *fakeConn) SetReadDeadline(_ time.Time) error  { return nil }
func (f *fakeConn) SetWriteDeadline(_ time.Time) error { return nil }

func newTestConn(c net.Conn) *Conn { return &Conn{Conn: c, br: bufio.NewReader(c)} }

// serveOne accepts exactly one connection on a fresh loopback listener wrapped
// by New, returning the wrapped server conn and a func that writes raw bytes
// from the client side. The genuine TCP peer of the returned conn is loopback,
// so the trust gate is satisfied (mirrors the in-container router → MDA dial).
func serveOne(t *testing.T, clientWrite []byte) net.Conn {
	t.Helper()
	return serveOneAuthed(t, clientWrite, nil)
}

// serveOneAuthed is serveOne with a router-auth secret configured on the
// listener (nil = the non-router-fronted box that trusts any loopback
// header).
func serveOneAuthed(t *testing.T, clientWrite, secret []byte) net.Conn {
	t.Helper()
	inner, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	t.Cleanup(func() { _ = inner.Close() })
	var opts []Option
	if secret != nil {
		opts = append(opts, WithRouterAuth(secret))
	}
	ln := New(inner, opts...)

	writeErr := make(chan error, 1)
	go func() {
		c, err := net.Dial("tcp", inner.Addr().String())
		if err != nil {
			writeErr <- err
			return
		}
		_, err = c.Write(clientWrite)
		writeErr <- err
		// Hold the conn open so the server side can read; closed by GC/test end.
		t.Cleanup(func() { _ = c.Close() })
	}()

	sc, err := ln.Accept()
	if err != nil {
		t.Fatalf("accept: %v", err)
	}
	t.Cleanup(func() { _ = sc.Close() })
	if err := <-writeErr; err != nil {
		t.Fatalf("client write: %v", err)
	}
	return sc
}

func TestSignatureMatchesSpec(t *testing.T) {
	// Must stay byte-identical to fauna_proxy_protocol::SIGNATURE (the Rust
	// router side) — \r\n\r\n\0\r\nQUIT\n.
	want := [12]byte{0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A}
	if Signature != want {
		t.Fatalf("Signature = %v, want %v", Signature, want)
	}
}

// TestGoldenV4 pins the exact 28 bytes fauna_proxy_protocol::encode_v2 emits
// for 203.0.113.7:54321 → 198.51.100.1:443, so a future edit to the field
// layout that silently diverges from the Rust router is caught here.
func TestGoldenV4(t *testing.T) {
	golden := []byte{
		0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A, // signature
		0x21,       // v2 + PROXY
		0x11,       // AF_INET + STREAM
		0x00, 0x0C, // addr block len = 12
		0xCB, 0x00, 0x71, 0x07, // src 203.0.113.7
		0xC6, 0x33, 0x64, 0x01, // dst 198.51.100.1
		0xD4, 0x31, // sport 54321
		0x01, 0xBB, // dport 443
	}
	src := &net.TCPAddr{IP: net.ParseIP("203.0.113.7"), Port: 54321}
	dst := &net.TCPAddr{IP: net.ParseIP("198.51.100.1"), Port: 443}
	if got := encodeV2ForTest(src, dst); !bytes.Equal(got, golden) {
		t.Fatalf("encodeV2ForTest =\n%x\nwant\n%x", got, golden)
	}
	info, ok := parsePrefix(golden[:PrefixLen])
	if !ok || !info.isProxyCommand || info.addrLen != tcp4BlockLen {
		t.Fatalf("parsePrefix(golden) = %+v ok=%v", info, ok)
	}
	gotSrc, ok := parseSrc(info.famProto, golden[PrefixLen:])
	if !ok || gotSrc.String() != "203.0.113.7:54321" {
		t.Fatalf("parseSrc(golden) = %v ok=%v, want 203.0.113.7:54321", gotSrc, ok)
	}
}

func TestParseV4RoundtripOverConn(t *testing.T) {
	src := &net.TCPAddr{IP: net.ParseIP("203.0.113.7"), Port: 54321}
	dst := &net.TCPAddr{IP: net.ParseIP("198.51.100.1"), Port: 443}
	payload := []byte{0x16, 0x03, 0x01, 0xDE, 0xAD} // a ClientHello-ish blob after the header
	sc := serveOne(t, append(encodeV2ForTest(src, dst), payload...))

	// Read triggers the lazy header parse; the bytes returned are the payload.
	got := make([]byte, len(payload))
	if _, err := io.ReadFull(sc, got); err != nil {
		t.Fatalf("read payload: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Fatalf("payload after header = %x, want %x", got, payload)
	}
	if sc.RemoteAddr().String() != "203.0.113.7:54321" {
		t.Fatalf("RemoteAddr = %v, want 203.0.113.7:54321", sc.RemoteAddr())
	}
}

func TestParseV6RoundtripOverConn(t *testing.T) {
	src := &net.TCPAddr{IP: net.ParseIP("2001:db8::dead:beef"), Port: 443}
	dst := &net.TCPAddr{IP: net.ParseIP("2001:db8::1"), Port: 443}
	payload := []byte("hello-after-header")
	sc := serveOne(t, append(encodeV2ForTest(src, dst), payload...))

	got := make([]byte, len(payload))
	if _, err := io.ReadFull(sc, got); err != nil {
		t.Fatalf("read payload: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Fatalf("payload after header = %q, want %q", got, payload)
	}
	if sc.RemoteAddr().String() != "[2001:db8::dead:beef]:443" {
		t.Fatalf("RemoteAddr = %v, want [2001:db8::dead:beef]:443", sc.RemoteAddr())
	}
}

// TestNoHeaderKeepsPeerAndBytes: a direct connection whose first byte is a TLS
// ClientHello (0x16), not the PROXY signature, keeps its genuine loopback peer
// and the bytes stay intact for the TLS acceptor.
func TestNoHeaderKeepsPeerAndBytes(t *testing.T) {
	clientHello := []byte{0x16, 0x03, 0x01, 0x00, 0x2A, 0x01}
	sc := serveOne(t, clientHello)
	genuine := sc.RemoteAddr() // captured before any Read; should be the dialer's loopback addr

	got := make([]byte, len(clientHello))
	if _, err := io.ReadFull(sc, got); err != nil {
		t.Fatalf("read: %v", err)
	}
	if !bytes.Equal(got, clientHello) {
		t.Fatalf("bytes = %x, want intact ClientHello %x", got, clientHello)
	}
	host, _, err := net.SplitHostPort(genuine.String())
	if err != nil {
		t.Fatalf("split: %v", err)
	}
	if !net.ParseIP(host).IsLoopback() {
		t.Fatalf("RemoteAddr host = %q, want a loopback addr (no header consumed)", host)
	}
}

// TestNonLoopbackPeerSkipsParse: even a valid PROXY header is ignored when the
// immediate TCP peer is NOT loopback — only the trusted in-container router may
// rewrite the source address. The genuine peer and the bytes are preserved.
func TestNonLoopbackPeerSkipsParse(t *testing.T) {
	src := &net.TCPAddr{IP: net.ParseIP("203.0.113.7"), Port: 54321}
	dst := &net.TCPAddr{IP: net.ParseIP("198.51.100.1"), Port: 443}
	header := encodeV2ForTest(src, dst)
	external := &net.TCPAddr{IP: net.ParseIP("203.0.113.9"), Port: 1234}
	fc := &fakeConn{r: bytes.NewReader(header), remote: external}
	c := newTestConn(fc)

	if c.RemoteAddr().String() != external.String() {
		t.Fatalf("RemoteAddr = %v, want untouched external peer %v", c.RemoteAddr(), external)
	}
	// The header bytes must NOT have been consumed (a non-router peer's bytes
	// are real payload).
	first := make([]byte, 1)
	if _, err := io.ReadFull(c, first); err != nil {
		t.Fatalf("read: %v", err)
	}
	if first[0] != Signature[0] {
		t.Fatalf("first byte = %#x, want header start %#x (bytes intact)", first[0], Signature[0])
	}
}

// ── router-auth TLV verification ──────────────────────────────────────
//
// The threat (docs/goal/architecture/security.md § Co-resident process trust
// boundary): the MTA/MDA/PDS bridges are ALSO loopback peers, so loopback alone
// no longer proves a PROXY header came from the SNI router. A compromised
// co-resident UID can dial the DAV/XRPC loopback port and forge a header to
// spoof a source IP — poisoning the CalDAV AUTH-lockout key (cross-user DoS),
// evading the coarse-tier password-spray brake, and laundering the audit. When a
// router-auth secret is provisioned, only a header carrying the matching TLV may
// rewrite the source.

// forgedSrc is the address a compromised co-resident process would claim.
var forgedSrc = &net.TCPAddr{IP: net.ParseIP("203.0.113.7"), Port: 54321}

var proxyDst = &net.TCPAddr{IP: net.ParseIP("198.51.100.1"), Port: 443}

// TestForgedHeaderWithoutAuthTLVKeepsGenuinePeer is the headline pin: a
// loopback peer that cannot read the root-owned secret sends a well-formed
// header with NO auth TLV; the conveyed source must be refused and the genuine
// loopback peer kept, so the forger gains nothing.
func TestForgedHeaderWithoutAuthTLVKeepsGenuinePeer(t *testing.T) {
	secret := []byte("a-32-byte-ish-router-auth-secret")
	payload := []byte{0x16, 0x03, 0x01, 0xDE, 0xAD}
	sc := serveOneAuthed(t, append(encodeV2ForTest(forgedSrc, proxyDst), payload...), secret)

	if sc.RemoteAddr().String() == forgedSrc.String() {
		t.Fatalf("RemoteAddr = %v — an unauthenticated header spoofed the source", sc.RemoteAddr())
	}
	host, _, err := net.SplitHostPort(sc.RemoteAddr().String())
	if err != nil {
		t.Fatalf("split: %v", err)
	}
	if !net.ParseIP(host).IsLoopback() {
		t.Fatalf("RemoteAddr host = %q, want the genuine loopback peer", host)
	}
	// The header is still CONSUMED — the stream must be positioned at the
	// ClientHello for the TLS acceptor either way (nest does the same).
	got := make([]byte, len(payload))
	if _, err := io.ReadFull(sc, got); err != nil {
		t.Fatalf("read payload: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Fatalf("payload after refused header = %x, want %x", got, payload)
	}
}

// TestForgedHeaderWithWrongAuthTLVKeepsGenuinePeer: a guessed/stale secret is
// refused exactly like an absent one.
func TestForgedHeaderWithWrongAuthTLVKeepsGenuinePeer(t *testing.T) {
	secret := []byte("a-32-byte-ish-router-auth-secret")
	wrong := []byte("a-32-byte-ish-router-auth-secreT") // one bit off
	header := encodeV2AuthedForTest(forgedSrc, proxyDst, wrong)
	sc := serveOneAuthed(t, header, secret)

	if sc.RemoteAddr().String() == forgedSrc.String() {
		t.Fatalf("RemoteAddr = %v — a wrong-secret header spoofed the source", sc.RemoteAddr())
	}
	host, _, _ := net.SplitHostPort(sc.RemoteAddr().String())
	if !net.ParseIP(host).IsLoopback() {
		t.Fatalf("RemoteAddr host = %q, want the genuine loopback peer", host)
	}
}

// TestAuthedHeaderFromRouterIsTrusted is the positive control: the real router
// appends the matching TLV, so the conveyed client IP still reaches RemoteAddr —
// without this the fix would break client-IP propagation outright.
func TestAuthedHeaderFromRouterIsTrusted(t *testing.T) {
	secret := []byte("a-32-byte-ish-router-auth-secret")
	payload := []byte{0x16, 0x03, 0x01, 0xDE, 0xAD}
	sc := serveOneAuthed(t, append(encodeV2AuthedForTest(forgedSrc, proxyDst, secret), payload...), secret)

	if sc.RemoteAddr().String() != forgedSrc.String() {
		t.Fatalf("RemoteAddr = %v, want the conveyed %v (a correctly authed header must be trusted)",
			sc.RemoteAddr(), forgedSrc)
	}
	got := make([]byte, len(payload))
	if _, err := io.ReadFull(sc, got); err != nil {
		t.Fatalf("read payload: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Fatalf("payload after authed header = %x, want %x (TLV bytes not fully consumed)", got, payload)
	}
}

// TestNoSecretConfiguredTrustsLoopback is the deploy-safety control: a box
// that provisioned no secret (non-router deployment, or a provisioning gap)
// keeps the trust-any-loopback behaviour. Failing closed there would
// collapse every external client to the router's loopback address — worse than
// the spoofing risk, and against works-out-of-the-box. Mirrors nest's
// load_router_auth_secret returning None.
func TestNoSecretConfiguredTrustsLoopback(t *testing.T) {
	sc := serveOneAuthed(t, encodeV2ForTest(forgedSrc, proxyDst), nil)
	if sc.RemoteAddr().String() != forgedSrc.String() {
		t.Fatalf("RemoteAddr = %v, want the conveyed %v (a listener with no secret trusts the loopback header)",
			sc.RemoteAddr(), forgedSrc)
	}
}

// TestAuthTLVParse covers the TLV walk itself, mirroring
// fauna_proxy_protocol::parse_router_auth_tlv's cases 1:1.
func TestAuthTLVParse(t *testing.T) {
	secret := []byte("s3cr3t")
	t.Run("found after an unrelated tlv", func(t *testing.T) {
		payload := make([]byte, tcp4BlockLen)
		payload = append(payload, 0x03, 0x00, 0x04, 0xAA, 0xBB, 0xCC, 0xDD) // PP2_TYPE_CRC32C
		payload = append(payload, tlvTypeRouterAuth, 0x00, byte(len(secret)))
		payload = append(payload, secret...)
		got, ok := parseRouterAuthTLV(famTCP4, payload)
		if !ok || !bytes.Equal(got, secret) {
			t.Fatalf("parseRouterAuthTLV = %q ok=%v, want %q", got, ok, secret)
		}
	})
	t.Run("absent when only other tlvs present", func(t *testing.T) {
		payload := make([]byte, tcp4BlockLen)
		payload = append(payload, 0x03, 0x00, 0x04, 0xAA, 0xBB, 0xCC, 0xDD)
		if _, ok := parseRouterAuthTLV(famTCP4, payload); ok {
			t.Fatalf("parseRouterAuthTLV found an auth TLV that is not there")
		}
	})
	t.Run("malformed length is absent, not trusted", func(t *testing.T) {
		payload := make([]byte, tcp4BlockLen)
		payload = append(payload, tlvTypeRouterAuth, 0xFF, 0xFF, 0x01) // claims 65535 bytes
		if _, ok := parseRouterAuthTLV(famTCP4, payload); ok {
			t.Fatalf("parseRouterAuthTLV trusted a malformed TLV length")
		}
	})
	t.Run("unsupported family", func(t *testing.T) {
		if _, ok := parseRouterAuthTLV(0x00, []byte{1, 2, 3}); ok {
			t.Fatalf("parseRouterAuthTLV accepted an unsupported family")
		}
	})
	t.Run("truncated block", func(t *testing.T) {
		if _, ok := parseRouterAuthTLV(famTCP4, []byte{1, 2, 3}); ok {
			t.Fatalf("parseRouterAuthTLV accepted a truncated address block")
		}
	})
}

// TestGoldenAuthedV4 pins the exact bytes fauna_proxy_protocol::encode_v2_authed
// emits for 203.0.113.7:54321 → 198.51.100.1:443 with a 4-byte secret, so a
// future edit to either side that silently diverges on the TLV layout is caught
// here (the sibling of TestGoldenV4, which pins the plain header).
func TestGoldenAuthedV4(t *testing.T) {
	golden := []byte{
		0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A, // signature
		0x21,       // v2 + PROXY
		0x11,       // AF_INET + STREAM
		0x00, 0x13, // len = 12 (addr block) + 3 (TLV header) + 4 (secret) = 19
		0xCB, 0x00, 0x71, 0x07, // src 203.0.113.7
		0xC6, 0x33, 0x64, 0x01, // dst 198.51.100.1
		0xD4, 0x31, // sport 54321
		0x01, 0xBB, // dport 443
		0xE0,       // TLV type = router-auth (spec private range 0xE0..=0xEF)
		0x00, 0x04, // TLV length = 4
		0xDE, 0xAD, 0xBE, 0xEF, // the secret
	}
	secret := []byte{0xDE, 0xAD, 0xBE, 0xEF}
	if got := encodeV2AuthedForTest(forgedSrc, proxyDst, secret); !bytes.Equal(got, golden) {
		t.Fatalf("encodeV2AuthedForTest =\n%x\nwant\n%x", got, golden)
	}
	info, ok := parsePrefix(golden[:PrefixLen])
	if !ok || !info.isProxyCommand || info.addrLen != tcp4BlockLen+tlvHeaderLen+len(secret) {
		t.Fatalf("parsePrefix(golden) = %+v ok=%v", info, ok)
	}
	// The address block must still resolve with a trailing TLV…
	gotSrc, ok := parseSrc(info.famProto, golden[PrefixLen:])
	if !ok || gotSrc.String() != "203.0.113.7:54321" {
		t.Fatalf("parseSrc(golden authed) = %v ok=%v", gotSrc, ok)
	}
	// …and the secret must round-trip.
	gotSecret, ok := parseRouterAuthTLV(info.famProto, golden[PrefixLen:])
	if !ok || !bytes.Equal(gotSecret, secret) {
		t.Fatalf("parseRouterAuthTLV(golden) = %x ok=%v, want %x", gotSecret, ok, secret)
	}
}

func TestParsePrefixRejectsBadInput(t *testing.T) {
	cases := []struct {
		name string
		mut  func(p []byte)
	}{
		{"tls clienthello", func(p []byte) { p[0] = 0x16 }},
		{"corrupt signature", func(p []byte) { copy(p, Signature[:]); p[5] ^= 0xFF }},
		{"wrong version", func(p []byte) { copy(p, Signature[:]); p[12] = 0x11 }}, // v1
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			p := make([]byte, PrefixLen)
			tc.mut(p)
			if _, ok := parsePrefix(p); ok {
				t.Fatalf("parsePrefix accepted %s", tc.name)
			}
		})
	}
}

func TestLocalCommandFlaggedNotProxy(t *testing.T) {
	p := make([]byte, PrefixLen)
	copy(p, Signature[:])
	p[12] = 0x20 // v2, LOCAL command
	info, ok := parsePrefix(p)
	if !ok {
		t.Fatalf("parsePrefix rejected a v2 LOCAL prefix")
	}
	if info.isProxyCommand {
		t.Fatalf("LOCAL command flagged as PROXY")
	}
}

func TestParseSrcTruncatedReturnsFalse(t *testing.T) {
	if _, ok := parseSrc(famTCP4, []byte{1, 2, 3}); ok {
		t.Fatalf("parseSrc accepted a truncated TCP4 block")
	}
	if _, ok := parseSrc(0x00, make([]byte, 64)); ok {
		t.Fatalf("parseSrc accepted an unsupported family")
	}
}
