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

// handshake runs one server handshake under HardenedServerConfig against a
// client config and returns the client's error (nil on success).
func handshake(t *testing.T, client *crypto_tls.Config) error {
	t.Helper()
	cert := selfSignedP256(t)
	srvCfg := HardenedServerConfig(func(*crypto_tls.ClientHelloInfo) (*crypto_tls.Certificate, error) {
		return cert, nil
	})
	cConn, sConn := net.Pipe()
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
	wantCurves := []crypto_tls.CurveID{crypto_tls.X25519, crypto_tls.CurveP256, crypto_tls.CurveP384}
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
