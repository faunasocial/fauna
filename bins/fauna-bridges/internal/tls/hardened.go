package tls

import crypto_tls "crypto/tls"

// pinnedCipherSuites is the TLS 1.2 suite list every public listener of the
// bridge offers, in server-preference order: ECDHE key exchange and AEAD
// ciphers only — no CBC mode (BEAST / Lucky13 class) and no static-RSA key
// exchange. TLS 1.3 suites are not configurable in crypto/tls and are all
// AEAD already. Owner: docs/goal/behavior/smtp-server.md § TLS posture per port.
var pinnedCipherSuites = []uint16{
	crypto_tls.TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
	crypto_tls.TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
	crypto_tls.TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
	crypto_tls.TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
	crypto_tls.TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
	crypto_tls.TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
}

// pinnedCurves is the key-exchange group list, in server-preference order:
// the post-quantum hybrids first, then the classical curves, P-521 dropped
// (slow, and no security upside on a public-internet listener).
//
// crypto/tls offers its default groups — the hybrids included — only while
// CurvePreferences is nil; once a list is set, only the listed groups are
// offered. So this explicit list MUST name every hybrid the toolchain's
// default carries, or post-quantum key exchange silently disappears from
// every listener (TestHardenedServerConfigKeepsEveryDefaultHybrid reddens
// when a Go bump adds one). Owners: docs/goal/behavior/smtp-server.md
// § TLS posture per port (the list), docs/goal/architecture/security/
// post-quantum.md (the PQ posture).
var pinnedCurves = []crypto_tls.CurveID{
	crypto_tls.X25519MLKEM768,
	crypto_tls.SecP256r1MLKEM768,
	crypto_tls.SecP384r1MLKEM1024,
	crypto_tls.X25519,
	crypto_tls.CurveP256,
	crypto_tls.CurveP384,
}

// HardenedServerConfig is the one server-side *tls.Config every TLS listener
// of the bridge binaries is built from (the MTA's 25/465/587, the MDA's
// IMAP + CalDAV, the PDS): a TLS 1.2 floor, the pinned AEAD-only suite list
// and the pinned curves, with getCert (normally Provider.GetCertificate)
// supplying the certificate. Each call returns a fresh value, so a listener
// that adds to its copy never reaches another's.
func HardenedServerConfig(getCert func(*crypto_tls.ClientHelloInfo) (*crypto_tls.Certificate, error)) *crypto_tls.Config {
	return &crypto_tls.Config{
		GetCertificate:   getCert,
		MinVersion:       crypto_tls.VersionTLS12,
		CipherSuites:     append([]uint16(nil), pinnedCipherSuites...),
		CurvePreferences: append([]crypto_tls.CurveID(nil), pinnedCurves...),
	}
}
