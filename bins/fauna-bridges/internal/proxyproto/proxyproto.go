// Package proxyproto peels an optional PROXY-protocol-v2 header off an accepted
// connection and reports the conveyed real client address as the conn's
// RemoteAddr. It is the Go MDA analogue of the Rust libs/fauna-proxy-protocol
// crate (the nest + router side); the two MUST stay wire-compatible — same
// 12-byte signature, same v2 PROXY-command / AF_INET / AF_INET6 / STREAM
// subset.
//
// # Why this exists
//
// The MDA's CalDAV listener binds loopback (127.0.0.1:8444) behind the
// fauna-sni-router, an L4 SNI-passthrough splicer that owns the container's
// public :443. A plain L4 splice makes the MDA see the router's loopback
// address as the peer for every internet client, which collapses the AUTH
// lockout's cross-account per-IP keying and the report_auth_event audit
// (docs/goal/behavior/caldav-server.md § Network exposure — Client-IP
// propagation). The router prepends a PROXY-v2 header (--send-proxy-to
// 127.0.0.1:8444) conveying the real client address; this package peels it
// BELOW tls.NewListener (and below the internal/connlimit wrap) so the real IP
// flows into the existing clientIP(r)/r.RemoteAddr seam with no handler change.
//
// # Trust model
//
// Mirrors nest's read_optional_proxy_header: the header is parsed and trusted
// ONLY when the immediate TCP peer is loopback (the in-container router). A
// connection whose first byte is not the PROXY signature (e.g. a TLS
// ClientHello, 0x16) keeps its genuine peer and its bytes intact, so a direct
// (headerless) connection is unaffected. The peel is lazy (on the first Read or
// RemoteAddr, whichever the TLS/HTTP stack reaches first) so it never blocks the
// accept loop, and bounded by a read deadline so a stalled header cannot pin the
// goroutine.
//
// Router-distinguished trust. Loopback alone is NOT enough: after the
// co-resident UID split the sibling bridges (fauna-mta, and each bridge relative
// to the others) are also loopback peers, so a compromised one could forge a
// header to spoof a source IP — poisoning the CalDAV AUTH-lockout key (a
// cross-user denial of service), evading the coarse-tier password-spray brake,
// and laundering the per-IP audit. So when a router-auth secret is provisioned
// (WithRouterAuth), a header is honoured only if it carries a matching
// TLV_TYPE_ROUTER_AUTH TLV — the router writes a secret from a root-owned file
// no bridge UID can read. A missing/wrong secret falls back to the genuine
// loopback peer, never the spoofed address. With no secret configured (a
// non-router box, or a provisioning gap) the trust-any-loopback
// behaviour stands, deliberately permissive: failing closed there would collapse
// every external client onto the router's loopback address, which is worse than
// the spoofing risk and against works-out-of-the-box. This mirrors nest's
// read_optional_proxy_header / load_router_auth_secret exactly. See
// docs/goal/architecture/security.md § Co-resident process trust boundary.
package proxyproto

import (
	"bufio"
	"bytes"
	"crypto/subtle"
	"encoding/binary"
	"encoding/hex"
	"log/slog"
	"net"
	"os"
	"strings"
	"sync"
	"time"
)

// Signature is the 12-byte PROXY-v2 prefix (\r\n\r\n\0\r\nQUIT\n). Byte-for-byte
// identical to fauna_proxy_protocol::SIGNATURE on the Rust router side; the
// first byte (0x0D) cheaply distinguishes a header from a TLS ClientHello
// (0x16).
var Signature = [12]byte{0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A}

// PrefixLen is the fixed header prefix: 12-byte signature + 1 version/command
// byte + 1 family/transport byte + 2-byte big-endian address-block length.
const PrefixLen = 16

const (
	verCmdProxy  = 0x21 // version 2 (high nibble) + PROXY command (low nibble)
	famTCP4      = 0x11 // AF_INET + STREAM
	famTCP6      = 0x21 // AF_INET6 + STREAM
	tcp4BlockLen = 12   // src(4) + dst(4) + sport(2) + dport(2)
	tcp6BlockLen = 36   // src(16) + dst(16) + sport(2) + dport(2)

	// headerTimeout bounds a slow/stalled header read; matches nest's 10s.
	headerTimeout = 10 * time.Second
	// maxAddrLen defensively caps the declared length of the address block plus
	// any trailing TLVs — including the router-auth TLV the router appends after
	// the 12/36-byte block (parseRouterAuthTLV reads it, parseSrc reads
	// only the leading block, and the payload is Discard-ed whole either way).
	// Matches nest's 1024 cap.
	maxAddrLen = 1024

	// tlvTypeRouterAuth is the PROXY-v2 TLV type carrying the router→backend
	// authentication secret. Byte-identical to
	// fauna_proxy_protocol::TLV_TYPE_ROUTER_AUTH on the Rust router/nest side;
	// the value sits in the v2 spec's 0xE0..=0xEF private range.
	tlvTypeRouterAuth = 0xE0
	// tlvHeaderLen is what one TLV adds beyond its value: type(1) + length(2 BE).
	tlvHeaderLen = 3
)

type prefixInfo struct {
	famProto       byte
	addrLen        int
	isProxyCommand bool
}

// parsePrefix parses the fixed 16-byte prefix. ok is false when the bytes are
// not a PROXY-v2 header we understand (wrong signature or version) — the caller
// then treats the connection as a direct, headerless one. p must be >= PrefixLen.
func parsePrefix(p []byte) (prefixInfo, bool) {
	if !bytes.Equal(p[:12], Signature[:]) {
		return prefixInfo{}, false
	}
	if p[12]>>4 != 0x2 {
		return prefixInfo{}, false
	}
	return prefixInfo{
		famProto:       p[13],
		addrLen:        int(binary.BigEndian.Uint16(p[14:16])),
		isProxyCommand: p[12]&0x0F == 0x1,
	}, true
}

// parseSrc parses the source TCP address from a PROXY-v2 address block given its
// family/transport byte. ok is false for unsupported families or a block too
// short to hold the declared family's addresses.
func parseSrc(famProto byte, block []byte) (*net.TCPAddr, bool) {
	switch famProto {
	case famTCP4:
		if len(block) < tcp4BlockLen {
			return nil, false
		}
		ip := net.IPv4(block[0], block[1], block[2], block[3])
		port := binary.BigEndian.Uint16(block[8:10])
		return &net.TCPAddr{IP: ip, Port: int(port)}, true
	case famTCP6:
		if len(block) < tcp6BlockLen {
			return nil, false
		}
		ip := make(net.IP, net.IPv6len)
		copy(ip, block[0:16])
		port := binary.BigEndian.Uint16(block[32:34])
		return &net.TCPAddr{IP: ip, Port: int(port)}, true
	default:
		return nil, false
	}
}

// parseRouterAuthTLV extracts the value of the router-auth TLV from a PROXY-v2
// payload (the addrLen-byte run after the fixed prefix), given its
// family/transport byte. The address block is skipped, then the trailing TLVs
// are walked; the first router-auth TLV's value is returned.
//
// ok is false when the family is unsupported, the block is truncated, no
// router-auth TLV is present, or a TLV length runs past the payload (malformed —
// treated as absent rather than trusted). The Go analogue of
// fauna_proxy_protocol::parse_router_auth_tlv; the two MUST stay in step.
func parseRouterAuthTLV(famProto byte, payload []byte) ([]byte, bool) {
	var blockLen int
	switch famProto {
	case famTCP4:
		blockLen = tcp4BlockLen
	case famTCP6:
		blockLen = tcp6BlockLen
	default:
		return nil, false
	}
	if len(payload) < blockLen {
		return nil, false
	}
	// Walk `type(1) || length(2 BE) || value` records.
	for tlvs := payload[blockLen:]; len(tlvs) >= tlvHeaderLen; {
		ty := tlvs[0]
		length := int(binary.BigEndian.Uint16(tlvs[1:3]))
		if len(tlvs) < tlvHeaderLen+length {
			return nil, false // malformed: never trusted
		}
		value := tlvs[tlvHeaderLen : tlvHeaderLen+length]
		if ty == tlvTypeRouterAuth {
			return value, true
		}
		tlvs = tlvs[tlvHeaderLen+length:]
	}
	return nil, false
}

// RouterAuthEnv is the IPC env carrying the router-auth secret, hex-encoded
// — the same name and encoding nest's load_router_auth_secret reads. The bridge
// run-scripts `cat /data/keys/router/proxy-secret` AS ROOT (before the
// s6-setuidgid drop) and export it: env, never argv, because /proc/<pid>/cmdline
// is world-readable while /proc/<pid>/environ is readable only by the process's
// own UID — so a co-resident bridge UID can read neither the root-owned 0600
// file nor another process's environ.
const RouterAuthEnv = "FAUNA_ROUTER_PROXY_SECRET"

// RouterAuthFromEnv loads the router-auth secret for a listener that is
// fronted by the in-container SNI router. Returns nil — keeping the
// "trust any loopback PROXY header" behaviour — when the artifact provisioned
// none (a non-router deployment, or a provisioning gap on a router box, which is
// logged loudly). Mirrors nest's load_router_auth_secret, including its
// deliberate permissiveness: failing closed would collapse every external client
// onto the router's loopback address, which is worse than the spoofing risk and
// against works-out-of-the-box.
func RouterAuthFromEnv(logger *slog.Logger) []byte {
	raw := strings.TrimSpace(os.Getenv(RouterAuthEnv))
	if raw == "" {
		return nil
	}
	secret, err := hex.DecodeString(raw)
	if err != nil || len(secret) == 0 {
		if logger != nil {
			logger.Error("router-auth secret is not valid non-empty hex — PROXY-v2 headers are NOT authenticated; a co-resident process could spoof a source IP",
				"env", RouterAuthEnv)
		}
		return nil
	}
	return secret
}

// Option configures a Listener.
type Option func(*Listener)

// WithRouterAuth requires a matching router-auth TLV before a conveyed
// PROXY-v2 source is honoured — see the package doc's trust model. An empty or
// nil secret is a no-op (trust-any-loopback), so a caller can pass an
// unprovisioned secret through unconditionally.
func WithRouterAuth(secret []byte) Option {
	return func(l *Listener) {
		if len(secret) > 0 {
			l.routerAuth = secret
		}
	}
}

// Listener wraps a net.Listener so each accepted connection peels an optional
// PROXY-v2 header. It overrides only Accept; Close, Addr, and the rest are
// promoted from the embedded net.Listener (so a graceful-drain path closing the
// wrapped listener closes the inner one unchanged).
type Listener struct {
	net.Listener
	// routerAuth is the secret a header's router-auth TLV must carry to be
	// trusted. nil ⇒ trust-any-loopback.
	routerAuth []byte
}

// New wraps inner so accepted connections report the PROXY-v2-conveyed client
// address (when sent by a trusted loopback upstream) as their RemoteAddr.
func New(inner net.Listener, opts ...Option) *Listener {
	l := &Listener{Listener: inner}
	for _, o := range opts {
		o(l)
	}
	return l
}

// Accept returns a *Conn that lazily peels the header on first use.
func (l *Listener) Accept() (net.Conn, error) {
	c, err := l.Listener.Accept()
	if err != nil {
		return nil, err
	}
	return &Conn{Conn: c, br: bufio.NewReader(c), routerAuth: l.routerAuth}, nil
}

// Conn reads through a bufio.Reader so the optional header can be peeked
// non-destructively (the Go analogue of nest's 1-byte stream.peek). Read and
// RemoteAddr both ensure the header is processed exactly once; everything else
// (Write, Close, deadlines) is promoted from the embedded net.Conn.
type Conn struct {
	net.Conn
	br         *bufio.Reader
	once       sync.Once
	realAddr   net.Addr
	routerAuth []byte
}

func (c *Conn) readHeader() {
	c.once.Do(func() {
		// Default: the genuine TCP peer. Overwritten only by a trusted header.
		c.realAddr = c.Conn.RemoteAddr()
		tcp, ok := c.Conn.RemoteAddr().(*net.TCPAddr)
		if !ok || !tcp.IP.IsLoopback() {
			return // only the in-container loopback router may rewrite the source
		}

		_ = c.Conn.SetReadDeadline(time.Now().Add(headerTimeout))
		defer func() { _ = c.Conn.SetReadDeadline(time.Time{}) }()

		// Peek the first byte without consuming: 0x0D ⇒ a PROXY header, anything
		// else (a TLS ClientHello's 0x16, an IMAP/SMTP greeting) ⇒ headerless,
		// leave every byte for the downstream reader.
		sig, err := c.br.Peek(1)
		if err != nil || sig[0] != Signature[0] {
			return
		}
		// First byte is the signature start ⇒ the router committed a full header.
		// From here we consume it; a malformed header from a (trusted) loopback
		// peer leaves the stream out of sync so the TLS handshake fails closed —
		// acceptable, as only a buggy loopback peer can reach this branch.
		prefix, err := c.br.Peek(PrefixLen)
		if err != nil {
			return
		}
		info, ok := parsePrefix(prefix)
		if !ok || !info.isProxyCommand || info.addrLen > maxAddrLen {
			return
		}
		full, err := c.br.Peek(PrefixLen + info.addrLen)
		if err != nil {
			return
		}
		if src, ok := parseSrc(info.famProto, full[PrefixLen:]); ok && c.headerIsTrusted(info.famProto, full[PrefixLen:]) {
			c.realAddr = src
		}
		// The header is consumed whether or not its source was honoured — the
		// stream must be positioned at the ClientHello for the TLS acceptor
		// either way (nest's read_optional_proxy_header does the same).
		_, _ = c.br.Discard(PrefixLen + info.addrLen)
	})
}

// headerIsTrusted reports whether a well-formed header from a loopback peer may
// actually rewrite the source address. With no secret provisioned this is
// the trust-any-loopback answer, true. With one, the header must carry a
// router-auth TLV matching it — compared in constant time, so a co-resident
// forger gets no timing oracle on the secret.
func (c *Conn) headerIsTrusted(famProto byte, payload []byte) bool {
	if len(c.routerAuth) == 0 {
		return true
	}
	got, ok := parseRouterAuthTLV(famProto, payload)
	return ok && subtle.ConstantTimeCompare(got, c.routerAuth) == 1
}

func (c *Conn) Read(p []byte) (int, error) {
	c.readHeader()
	return c.br.Read(p)
}

// RemoteAddr returns the PROXY-v2-conveyed client address when a trusted header
// was present, else the genuine TCP peer.
func (c *Conn) RemoteAddr() net.Addr {
	c.readHeader()
	return c.realAddr
}
