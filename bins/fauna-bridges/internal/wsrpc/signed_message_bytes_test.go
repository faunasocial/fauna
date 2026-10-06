// The Go half of the signed-message BYTE contract with Rust.
//
// # What this catches that the merge gate does not
//
// `go-signed-message-parity-check` reads Rust's `sig_domain::AUTH_VERIFY_V2`
// and the Go literal and reds when the two TAGS differ. That is the direction
// which took mail/IMAP/CalDAV down for ~16 h on 2026-08-17, and it runs
// synchronously on every merge on every machine. Nothing here replaces it.
//
// But a signed message is not only its tag. `ChallengeVerifySignedMessage` is
// `domain ‖ actorID ‖ nonce ‖ nestID` and `EnrollmentSignedMessage` is
// `domain ‖ ed25519 ‖ x25519 ‖ role`, and each has an independent Rust
// construction. **A change to the LAYOUT leaves both tag literals untouched**, so
// the gate stays green while the bridge signs bytes nest refuses.
//
// Not hypothetical: `sig_domain` already exports a second constructor,
// `domain_separated_length_prefixed`, which prefixes every element — the tag
// included — with a big-endian u64 length. Re-targeting either ceremony onto it
// is exactly the injectivity hardening a future session does. It would change
// every signed byte, keep the tag spelled identically, and take every bridge role
// off the air.
//
// # Why the vectors are not written here
//
// The fixture carries its own INPUTS, so this test reads actor/nonce/pubkeys/role
// from it and feeds them to the production builders. A hand-typed vector on this
// side would be a second copy that agrees with itself — the same trap as
// TestAuthClientSignatureRoundTrip, which hand-built the message, signed it, and
// verified it against the same hand-built bytes: a tautology asserting that
// ed25519 works, green straight through the sweep that broke production.
//
// The fixture is written by libs/fauna-protocol/tests/go_signed_message_bytes.rs,
// which regenerates it from the live Rust builders and reds when it is stale.
package wsrpc

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

type signedMessageVector struct {
	Ceremony      string `json:"ceremony"`
	ActorID       string `json:"actor_id"`
	Nonce         string `json:"nonce"`
	NestID        string `json:"nest_id"`
	SpkiSha256    string `json:"spki_sha256"`
	ClientNonce   string `json:"client_nonce"`
	Role          string `json:"role"`
	Ed25519Pubkey string `json:"ed25519_pubkey"`
	X25519Pubkey  string `json:"x25519_pubkey"`
	SignedMessage string `json:"signed_message"`
}

func loadSignedMessageVectors(t *testing.T) []signedMessageVector {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("testdata", "signed-message-bytes.json"))
	if err != nil {
		t.Fatalf("read the signed-message fixture: %v\n\nIt is written by "+
			"libs/fauna-protocol/tests/go_signed_message_bytes.rs — run "+
			"`cargo test -p fauna-protocol --test go_signed_message_bytes` and follow "+
			"its failure, which prints the exact bytes.", err)
	}
	var vectors []signedMessageVector
	if err := json.Unmarshal(raw, &vectors); err != nil {
		t.Fatalf("parse the signed-message fixture: %v", err)
	}
	if len(vectors) == 0 {
		t.Fatal("the signed-message fixture is empty — a vacuous pass is not coverage")
	}
	return vectors
}

func mustHex(t *testing.T, field, s string) []byte {
	t.Helper()
	if s == "" {
		t.Fatalf("fixture vector is missing %s", field)
	}
	b, err := hex.DecodeString(s)
	if err != nil {
		t.Fatalf("fixture %s is not hex: %v", field, err)
	}
	return b
}

// TestSignedMessageBytesMatchRust drives the PRODUCTION builders over
// Rust-generated vectors and compares the bytes.
//
// A red here means the bridge would sign something nest refuses: every role (MTA,
// MDA, CalDAV) enrols and then fails its first bearer acquisition with
// fauna.auth.signature_failed. Fix the Go builder to match Rust — or, if the Rust
// change was deliberate, change both sides in the SAME commit.
func TestSignedMessageBytesMatchRust(t *testing.T) {
	seen := map[string]int{}
	for i, v := range loadSignedMessageVectors(t) {
		want := mustHex(t, "signed_message", v.SignedMessage)
		var got []byte

		switch v.Ceremony {
		case "challenge_verify":
			got = ChallengeVerifySignedMessage(
				mustHex(t, "actor_id", v.ActorID),
				mustHex(t, "nonce", v.Nonce),
				mustHex(t, "nest_id", v.NestID),
			)
		case "cert_binding":
			// The SPKI may legitimately be EMPTY (a plaintext nest), so it is
			// decoded without the non-empty guard.
			spki, err := hex.DecodeString(v.SpkiSha256)
			if err != nil {
				t.Fatalf("vector %d: spki_sha256 is not hex: %v", i, err)
			}
			got = CertBindingSignedMessage(spki, mustHex(t, "client_nonce", v.ClientNonce))
		case "enroll":
			if v.Role == "" {
				t.Fatalf("vector %d: enroll vector with no role", i)
			}
			got = EnrollmentSignedMessage(
				v.Role,
				mustHex(t, "ed25519_pubkey", v.Ed25519Pubkey),
				mustHex(t, "x25519_pubkey", v.X25519Pubkey),
			)
		default:
			// An unknown ceremony is a Rust side that grew a vector this test
			// cannot drive — which means a hand-rolled Go signer nothing checks.
			t.Fatalf("vector %d: unknown ceremony %q — add the case, do not skip it: "+
				"an undriven vector is an ungated signer", i, v.Ceremony)
		}
		seen[v.Ceremony]++

		if string(got) != string(want) {
			t.Errorf("%s vector %d: the Go builder does not produce Rust's bytes.\n"+
				"  got  %x\n  want %x\n\n"+
				"The domain tag alone may still match — `go-signed-message-parity-check` "+
				"cannot see a LAYOUT change, which is why this fixture exists.",
				v.Ceremony, i, got, want)
		}
	}

	// Both production ceremonies must actually be exercised. A fixture that
	// silently lost its enroll vectors would leave this test green over half the
	// surface.
	for _, ceremony := range []string{"challenge_verify", "cert_binding", "enroll"} {
		if seen[ceremony] == 0 {
			t.Errorf("no %s vector in the fixture — that ceremony is unpinned", ceremony)
		}
	}
}

// TestSignedMessageVectorsDistinguishTheirFields guards the guard: vectors whose
// fill bytes cannot tell two adjacent fields apart would let a TRANSPOSED Go
// builder match the fixture and pass.
func TestSignedMessageVectorsDistinguishTheirFields(t *testing.T) {
	for i, v := range loadSignedMessageVectors(t) {
		switch v.Ceremony {
		case "challenge_verify":
			if v.ActorID == v.Nonce {
				t.Errorf("vector %d: actor_id and nonce use the same fill, so a builder "+
					"that swaps them would still match", i)
			}
			if v.NestID == v.Nonce || v.NestID == v.ActorID {
				t.Errorf("vector %d: nest_id shares a fill with a neighbour, so a builder "+
					"that swaps them would still match", i)
			}
		case "cert_binding":
			if v.SpkiSha256 == v.ClientNonce {
				t.Errorf("vector %d: spki and client_nonce use the same fill, so a builder "+
					"that swaps them would still match", i)
			}
		case "enroll":
			if v.Ed25519Pubkey == v.X25519Pubkey {
				t.Errorf("vector %d: the two pubkeys use the same fill, so a builder that "+
					"swaps them would still match", i)
			}
		}
	}
}
