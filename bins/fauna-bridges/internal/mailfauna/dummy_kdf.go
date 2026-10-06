package mailfauna

import (
	"crypto/rand"
	"sync"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// Dummy-KDF timing equalizer (network-exposure.md § Rulings F3).
//
// The IMAP/CalDAV pre-auth paths return for an UNKNOWN user *before* any KDF
// (validate_recipient miss), while an existing user + wrong password runs the
// full Argon2id inside UnwrapMLSBlob. The wire text is uniform, but the
// KDF-vs-no-KDF timing gap distinguishes valid accounts from unknown ones — a
// remote user-enumeration oracle. DummyCredentialKDF makes the unknown-user
// path incur the identical Argon2id cost, closing the gap the auth code's own
// comments say it wants closed.
//
// It runs the SAME Rust Argon2id as a real unwrap (via UnwrapMLSBlob), against a
// once-minted dummy blob sealed with the library-default interactive params
// (65536 KiB, t=2, p=1) — byte-for-byte what a real user blob is minted with
// (fauna_mls::wrapped_blob::default_kdf_for) — so there is no residual
// implementation- or parameter-timing difference between the two paths.

var (
	dummyKDFOnce      sync.Once
	dummyArgon2idBlob []byte // sealed under the interactive Argon2id default; nil if minting failed
)

// A well-formed but meaningless actor_id / credential_id for the dummy blob.
// The MSEK is random (minted below), so no real credential can ever unwrap it.
var (
	dummyActorID      = make([]byte, 32) // 32 zero bytes: valid length, never a real actor
	dummyCredentialID = "dummy"
)

// mintDummyBlob seals a throwaway MSEK under a random-nothing credential using
// the default Argon2id (PLAIN) KDF params. Runs exactly one Argon2id (the seal's
// wrap-key derivation); cached for the process lifetime.
func mintDummyBlob() {
	msek := make([]byte, 32)
	if _, err := rand.Read(msek); err != nil {
		// crypto/rand failure is catastrophic and vanishingly rare; degrade the
		// timing mitigation to a no-op rather than crash the auth path.
		return
	}
	// credential_kind "plain" + nil kdf_params → the shared default_kdf_for
	// ("plain") → Argon2id interactive, matching every real user blob.
	blob, err := faunaFfi.SealWrappedMsekBlob(
		msek, dummyActorID, dummyCredentialID, "plain", []byte("x"), nil,
	)
	if err == nil {
		dummyArgon2idBlob = blob
	}
}

// PrewarmDummyKDF mints the dummy blob ahead of the first authentication so the
// first unknown-user probe does not pay a one-time seal-plus-unwrap (2×) cost.
// Safe to call multiple times and from any goroutine; a no-op after the first.
// Call it from MDA startup once mail serving is confirmed enabled.
func PrewarmDummyKDF() {
	dummyKDFOnce.Do(mintDummyBlob)
}

// DummyCredentialKDF runs the same credential KDF a real AUTH attempt runs, for
// a user/credential that does not exist, so an unknown-user AUTH-fail costs the
// same as an existing-user wrong-password AUTH-fail (network-exposure.md § F3).
// The result is discarded — this exists solely to consume the KDF time.
//
// Only the Argon2id (AUTH=PLAIN) arm has a meaningful timing gap; HKDF
// (OAUTHBEARER) is microsecond-cheap, so this is a no-op for it.
func DummyCredentialKDF(secret []byte, kind KdfKind) {
	if kind != KdfKindArgon2id {
		return
	}
	dummyKDFOnce.Do(mintDummyBlob)
	if dummyArgon2idBlob == nil {
		return
	}
	// Runs the real Argon2id over `secret`, then fails AEAD (random MSEK). The
	// success/failure is irrelevant — zeroize a capability on the (near-
	// impossible) success path and discard.
	if capability, err := UnwrapMLSBlob(
		dummyArgon2idBlob, secret, dummyActorID, dummyCredentialID, kind,
	); err == nil && capability != nil {
		capability.Zeroize()
	}
}
