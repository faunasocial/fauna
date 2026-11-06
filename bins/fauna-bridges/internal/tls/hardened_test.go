package tls

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	crypto_tls "crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"math/big"
	"net"
	"slices"
	"testing"
	"time"
)

func selfSignedP256(t *testing.T) *crypto_tls.Certificate {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatalf("keygen: %v", err)
	}
	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "mail.test.example"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		DNSNames:     []string{"mail.test.example"},
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		t.Fatalf("cert: %v", err)
	}
	return &crypto_tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}
}

// connPair returns the two ends of a loopback TCP connection. Not net.Pipe:
// a pipe is unbuffered, so a TLS 1.3 HelloRetryRequest (both peers writing
// their compatibility ChangeCipherSpec at once) deadlocks it, and a config
// that forces one would hang the test instead of failing it.
func connPair(t *testing.T) (client, server net.Conn) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()
	accepted := make(chan net.Conn, 1)
	go func() {
		c, _ := ln.Accept()
		accepted <- c
	}()
	client, err = net.Dial("tcp", ln.Addr().String())
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	server = <-accepted
	if server == nil {
		t.Fatal("accept failed")
	}
	return client, server
}

// handshake runs one server handshake under HardenedServerConfig against a
// client config and returns the client's error (nil on success).
func handshake(t *testing.T, client *crypto_tls.Config) error {
	t.Helper()
	cert := selfSignedP256(t)
	srvCfg := HardenedServerConfig(func(*crypto_tls.ClientHelloInfo) (*crypto_tls.Certificate, error) {
		return cert, nil
	})
	cConn, sConn := connPair(t)
	defer cConn.Close()
	defer sConn.Close()
	done := make(chan struct{})
	go func() {
		defer close(done)
		_ = crypto_tls.Server(sConn, srvCfg).Handshake()
		sConn.Close()
	}()
	client.InsecureSkipVerify = true //nolint:gosec // test-only self-signed peer
	err := crypto_tls.Client(cConn, client).Handshake()
	cConn.Close()
	<-done
	return err
}

func TestHardenedServerConfigPinsAEADSuitesAndCurves(t *testing.T) {
	cfg := HardenedServerConfig(nil)
	if cfg.MinVersion != crypto_tls.VersionTLS12 {
		t.Fatalf("MinVersion = %x, want TLS 1.2", cfg.MinVersion)
	}
	want := []uint16{
		crypto_tls.TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
		crypto_tls.TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
		crypto_tls.TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
		crypto_tls.TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
		crypto_tls.TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
		crypto_tls.TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
	}
	if !slices.Equal(cfg.CipherSuites, want) {
		t.Fatalf("CipherSuites = %v, want %v", cfg.CipherSuites, want)
	}
	wantCurves := []crypto_tls.CurveID{
		crypto_tls.X25519MLKEM768,
		crypto_tls.SecP256r1MLKEM768,
		crypto_tls.SecP384r1MLKEM1024,
		crypto_tls.X25519,
		crypto_tls.CurveP256,
		crypto_tls.CurveP384,
	}
	if !slices.Equal(cfg.CurvePreferences, wantCurves) {
		t.Fatalf("CurvePreferences = %v, want %v", cfg.CurvePreferences, wantCurves)
	}
	// Each call is a fresh value: a caller mutating its copy never reaches
	// another listener's.
	if HardenedServerConfig(nil) == cfg {
		t.Fatal("HardenedServerConfig returned a shared pointer")
	}
}

func TestHardenedServerConfigRefusesCBCOnlyClient(t *testing.T) {
	err := handshake(t, &crypto_tls.Config{
		MaxVersion:   crypto_tls.VersionTLS12,
		CipherSuites: []uint16{crypto_tls.TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA},
	})
	if err == nil {
		t.Fatal("a client offering only a CBC suite completed the handshake")
	}
}

func TestHardenedServerConfigRefusesTLS11Client(t *testing.T) {
	err := handshake(t, &crypto_tls.Config{
		MinVersion: crypto_tls.VersionTLS10,
		MaxVersion: crypto_tls.VersionTLS11,
	})
	if err == nil {
		t.Fatal("a TLS 1.1 client completed the handshake")
	}
}

func TestHardenedServerConfigAcceptsAEADClients(t *testing.T) {
	if err := handshake(t, &crypto_tls.Config{
		MaxVersion:   crypto_tls.VersionTLS12,
		CipherSuites: []uint16{crypto_tls.TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256},
	}); err != nil {
		t.Fatalf("TLS 1.2 AEAD client refused: %v", err)
	}
	if err := handshake(t, &crypto_tls.Config{MinVersion: crypto_tls.VersionTLS13}); err != nil {
		t.Fatalf("TLS 1.3 client refused: %v", err)
	}
}

// hybridHandshake runs one TLS 1.3 handshake under HardenedServerConfig with a
// client offering exactly `offer`, returning the negotiated group.
func hybridHandshake(t *testing.T, offer []crypto_tls.CurveID) (crypto_tls.CurveID, error) {
	t.Helper()
	cert := selfSignedP256(t)
	srvCfg := HardenedServerConfig(func(*crypto_tls.ClientHelloInfo) (*crypto_tls.Certificate, error) {
		return cert, nil
	})
	cConn, sConn := connPair(t)
	defer cConn.Close()
	defer sConn.Close()
	done := make(chan struct{})
	go func() {
		defer close(done)
		_ = crypto_tls.Server(sConn, srvCfg).Handshake()
		sConn.Close()
	}()
	c := crypto_tls.Client(cConn, &crypto_tls.Config{
		MinVersion:         crypto_tls.VersionTLS13,
		CurvePreferences:   offer,
		InsecureSkipVerify: true, //nolint:gosec // test-only self-signed peer
	})
	err := c.Handshake()
	cConn.Close()
	<-done
	return c.ConnectionState().CurveID, err
}

// Every post-quantum hybrid group must survive the explicit curve list: Go
// offers them by default only while CurvePreferences is nil, so pinning a
// list without them silently drops PQ key exchange on every listener.
func TestHardenedServerConfigNegotiatesPQHybrids(t *testing.T) {
	for _, hybrid := range []crypto_tls.CurveID{
		crypto_tls.X25519MLKEM768,
		crypto_tls.SecP256r1MLKEM768,
		crypto_tls.SecP384r1MLKEM1024,
	} {
		got, err := hybridHandshake(t, []crypto_tls.CurveID{hybrid})
		if err != nil {
			t.Fatalf("client offering only %v refused: %v", hybrid, err)
		}
		if got != hybrid {
			t.Fatalf("client offering only %v negotiated %v", hybrid, got)
		}
	}
	// A modern client offering the hybrid beside classical X25519 gets the hybrid.
	got, err := hybridHandshake(t, []crypto_tls.CurveID{crypto_tls.X25519MLKEM768, crypto_tls.X25519})
	if err != nil {
		t.Fatalf("hybrid+X25519 client refused: %v", err)
	}
	if got != crypto_tls.X25519MLKEM768 {
		t.Fatalf("hybrid+X25519 client negotiated %v, want X25519MLKEM768", got)
	}
}

// The pinned list keeps every hybrid group the toolchain's default offers, so
// a Go bump that adds a new default hybrid reddens here instead of being
// silently left out.
func TestHardenedServerConfigKeepsEveryDefaultHybrid(t *testing.T) {
	classical := map[crypto_tls.CurveID]bool{
		crypto_tls.X25519: true, crypto_tls.CurveP256: true,
		crypto_tls.CurveP384: true, crypto_tls.CurveP521: true,
	}
	offered := make(chan []crypto_tls.CurveID, 1)
	cert := selfSignedP256(t)
	srvCfg := &crypto_tls.Config{
		GetCertificate: func(hello *crypto_tls.ClientHelloInfo) (*crypto_tls.Certificate, error) {
			offered <- append([]crypto_tls.CurveID(nil), hello.SupportedCurves...)
			return cert, nil
		},
	}
	cConn, sConn := connPair(t)
	done := make(chan struct{})
	go func() {
		defer close(done)
		_ = crypto_tls.Server(sConn, srvCfg).Handshake()
		sConn.Close()
	}()
	_ = crypto_tls.Client(cConn, &crypto_tls.Config{InsecureSkipVerify: true}).Handshake() //nolint:gosec // test-only
	cConn.Close()
	<-done
	pinned := HardenedServerConfig(nil).CurvePreferences
	for _, id := range <-offered {
		if !classical[id] && !slices.Contains(pinned, id) {
			t.Errorf("default hybrid group %v missing from pinnedCurves %v", id, pinned)
		}
	}
}
