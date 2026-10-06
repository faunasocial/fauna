package atprotoid

import (
	"regexp"
	"strings"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/ipfs/go-cid"
)

// testKeys mints a user rotation key, a bridge rotation key, and a signing
// key (all K-256, the product default) and returns them with their did:key
// strings.
func testKeys(t *testing.T) (userKey, bridgeKey, signingKey atcrypto.PrivateKeyExportable, userDID, bridgeDID, signingDID string) {
	t.Helper()
	mk := func() (atcrypto.PrivateKeyExportable, string) {
		k, err := atcrypto.GeneratePrivateKeyK256()
		if err != nil {
			t.Fatalf("GeneratePrivateKeyK256: %v", err)
		}
		didKey, err := DIDKeyForPrivate(k)
		if err != nil {
			t.Fatalf("DIDKeyForPrivate: %v", err)
		}
		return k, didKey
	}
	userKey, userDID = mk()
	bridgeKey, bridgeDID = mk()
	signingKey, signingDID = mk()
	return
}

// TestGenesisOpSeniority is the S2-critical custody test: the USER-custodied
// rotation key sits at rotationKeys[0] (most senior — always able to recover
// from a bridge-key compromise within the 72h window), the bridge key
// strictly after it, and the genesis signature verifies against the BRIDGE
// rotation PUBLIC key (genesis may be signed by any key in its own
// rotationKeys; the user contributes only a pubkey at mint, never a
// signature). A tampered op must fail verification.
func TestGenesisOpSeniority(t *testing.T) {
	_, bridgeKey, _, userDID, bridgeDID, signingDID := testKeys(t)

	op := BuildGenesisOp(userDID, bridgeDID, signingDID, "alice.example.com", "https://example.com")

	if len(op.RotationKeys) != 2 {
		t.Fatalf("rotationKeys len = %d, want 2", len(op.RotationKeys))
	}
	if op.RotationKeys[0] != userDID {
		t.Errorf("rotationKeys[0] = %q, want the USER key %q (seniority invariant)", op.RotationKeys[0], userDID)
	}
	if op.RotationKeys[1] != bridgeDID {
		t.Errorf("rotationKeys[1] = %q, want the bridge key %q", op.RotationKeys[1], bridgeDID)
	}
	if op.Prev != nil {
		t.Errorf("genesis op prev = %v, want nil", *op.Prev)
	}

	if err := op.Sign(bridgeKey); err != nil {
		t.Fatalf("Sign: %v", err)
	}
	if op.Sig == "" {
		t.Fatal("Sign left Sig empty")
	}
	// base64url no padding: never '=', '+', or '/'.
	if strings.ContainsAny(op.Sig, "=+/") {
		t.Errorf("sig %q is not base64url-no-pad", op.Sig)
	}

	// Verify against the bridge rotation pubkey parsed from its did:key
	// string — the exact spelling nest's roster carries.
	bridgePub, err := ParsePublicDIDKey(bridgeDID)
	if err != nil {
		t.Fatalf("ParsePublicDIDKey: %v", err)
	}
	if err := op.VerifySig(bridgePub); err != nil {
		t.Errorf("signed genesis op does not verify against the bridge rotation pubkey: %v", err)
	}

	// The signature must NOT verify against the user key (it was not the signer).
	userPub, err := ParsePublicDIDKey(userDID)
	if err != nil {
		t.Fatalf("ParsePublicDIDKey(user): %v", err)
	}
	if err := op.VerifySig(userPub); err == nil {
		t.Error("signature verified against the NON-signing user key; want failure")
	}

	// Tampering with the signed content must break verification.
	tampered := *op
	tampered.AlsoKnownAs = []string{"at://mallory.example.com"}
	if err := tampered.VerifySig(bridgePub); err == nil {
		t.Error("tampered op still verifies; want failure")
	}
}

var plcDIDRe = regexp.MustCompile(`^did:plc:[a-z2-7]{24}$`)

// TestDerivePlcDid locks the identifier derivation: format
// ^did:plc:[a-z2-7]{24}$, deterministic for the same signed op, different for
// a different op.
func TestDerivePlcDid(t *testing.T) {
	_, bridgeKey, _, userDID, bridgeDID, signingDID := testKeys(t)

	op := BuildGenesisOp(userDID, bridgeDID, signingDID, "alice.example.com", "https://example.com")
	if err := op.Sign(bridgeKey); err != nil {
		t.Fatalf("Sign: %v", err)
	}
	signed, err := op.SignedCBOR()
	if err != nil {
		t.Fatalf("SignedCBOR: %v", err)
	}

	did := DerivePlcDid(signed)
	if !plcDIDRe.MatchString(did) {
		t.Errorf("derived DID %q does not match ^did:plc:[a-z2-7]{24}$", did)
	}
	if again := DerivePlcDid(signed); again != did {
		t.Errorf("derivation not deterministic: %q vs %q", did, again)
	}

	other := BuildGenesisOp(userDID, bridgeDID, signingDID, "bob.example.com", "https://example.com")
	if err := other.Sign(bridgeKey); err != nil {
		t.Fatalf("Sign(other): %v", err)
	}
	otherSigned, err := other.SignedCBOR()
	if err != nil {
		t.Fatalf("SignedCBOR(other): %v", err)
	}
	if otherDID := DerivePlcDid(otherSigned); otherDID == did {
		t.Errorf("different ops derived the same DID %q", did)
	}
}

// TestGenesisCid pins the CID shape (CIDv1, dag-cbor 0x71, sha2-256) and its
// determinism — the value `prev` chains on.
func TestGenesisCid(t *testing.T) {
	_, bridgeKey, _, userDID, bridgeDID, signingDID := testKeys(t)
	op := BuildGenesisOp(userDID, bridgeDID, signingDID, "alice.example.com", "https://example.com")
	if err := op.Sign(bridgeKey); err != nil {
		t.Fatalf("Sign: %v", err)
	}
	signed, err := op.SignedCBOR()
	if err != nil {
		t.Fatalf("SignedCBOR: %v", err)
	}

	cidStr, err := GenesisCid(signed)
	if err != nil {
		t.Fatalf("GenesisCid: %v", err)
	}
	c, err := cid.Decode(cidStr)
	if err != nil {
		t.Fatalf("GenesisCid %q does not decode: %v", cidStr, err)
	}
	if c.Version() != 1 {
		t.Errorf("cid version = %d, want 1", c.Version())
	}
	if c.Type() != 0x71 {
		t.Errorf("cid codec = %#x, want dag-cbor 0x71", c.Type())
	}
	again, err := GenesisCid(signed)
	if err != nil {
		t.Fatalf("GenesisCid(again): %v", err)
	}
	if again != cidStr {
		t.Errorf("cid not deterministic: %q vs %q", cidStr, again)
	}
}

// TestUnsignedCBORExcludesSig locks the signing input: the unsigned encoding
// must differ from the signed one (sig present) and be stable regardless of
// whether Sig is set on the struct.
func TestUnsignedCBORExcludesSig(t *testing.T) {
	_, bridgeKey, _, userDID, bridgeDID, signingDID := testKeys(t)
	op := BuildGenesisOp(userDID, bridgeDID, signingDID, "alice.example.com", "https://example.com")

	before, err := op.UnsignedCBOR()
	if err != nil {
		t.Fatalf("UnsignedCBOR: %v", err)
	}
	if err := op.Sign(bridgeKey); err != nil {
		t.Fatalf("Sign: %v", err)
	}
	after, err := op.UnsignedCBOR()
	if err != nil {
		t.Fatalf("UnsignedCBOR(after sign): %v", err)
	}
	if string(before) != string(after) {
		t.Error("UnsignedCBOR changed after signing; sig is leaking into the signing input")
	}
	signed, err := op.SignedCBOR()
	if err != nil {
		t.Fatalf("SignedCBOR: %v", err)
	}
	if string(signed) == string(before) {
		t.Error("SignedCBOR equals UnsignedCBOR; sig missing from the signed encoding")
	}
}

// TestBuildUpdateOpFromPrev locks the non-genesis shape: prev chained, only
// alsoKnownAs changed, and the signature verifies under the bridge's junior
// rotation key (the key a rename is signed with).
func TestBuildUpdateOpFromPrev(t *testing.T) {
	_, bridgeKey, _, userDID, bridgeDID, signingDID := testKeys(t)
	genesis := BuildGenesisOp(userDID, bridgeDID, signingDID, "alice.example.com", "https://example.com")
	if err := genesis.Sign(bridgeKey); err != nil {
		t.Fatalf("Sign(genesis): %v", err)
	}
	signed, err := genesis.SignedCBOR()
	if err != nil {
		t.Fatalf("SignedCBOR: %v", err)
	}
	prevCID, err := GenesisCid(signed)
	if err != nil {
		t.Fatalf("GenesisCid: %v", err)
	}

	update := BuildUpdateOpFromPrev(genesis, prevCID, "renamed.example.com")
	if update.Prev == nil || *update.Prev != prevCID {
		t.Fatalf("update op prev = %v, want %q", update.Prev, prevCID)
	}
	if update.RotationKeys[0] != userDID {
		t.Errorf("update op rotationKeys[0] = %q, want user key (seniority survives updates)", update.RotationKeys[0])
	}
	if got := PrimaryHandle(update); got != "renamed.example.com" {
		t.Errorf("update op primary handle = %q, want the renamed one", got)
	}
	if update.Sig != "" {
		t.Error("BuildUpdateOpFromPrev returned a signed op; it must clear the carried-forward sig")
	}
	if err := update.Sign(bridgeKey); err != nil {
		t.Fatalf("Sign(update): %v", err)
	}
	bridgePub, err := ParsePublicDIDKey(bridgeDID)
	if err != nil {
		t.Fatalf("ParsePublicDIDKey: %v", err)
	}
	if err := update.VerifySig(bridgePub); err != nil {
		t.Errorf("signed update op does not verify: %v", err)
	}
}

// TestBuildUpdateOpFromPrevCarriesForwardUserRotations is the custody
// regression: a user who rotated their signing key (or moved their PDS)
// through the PLC log without this box must not have it reverted by the next
// handle rename. The update op is a strict alsoKnownAs delta over the state
// the DIRECTORY holds — including fields nest's roster view never saw — and it
// deep-copies, so mutating the result cannot corrupt the fetched op.
func TestBuildUpdateOpFromPrevCarriesForwardUserRotations(t *testing.T) {
	_, _, _, userDID, bridgeDID, signingDID := testKeys(t)
	// The head of the log as the directory has it: the user rotated the atproto
	// signing key to one this bridge has never held, and moved the PDS.
	head := BuildGenesisOp(userDID, bridgeDID, signingDID, "alice.example.com", "https://old.example.com")
	head.VerificationMethods["atproto"] = "did:key:zUserRotatedToThis"
	head.Services["atproto_pds"] = PlcService{Type: "AtprotoPersonalDataServer", Endpoint: "https://elsewhere.example.com"}
	head.Sig = "carried-forward-signature-must-be-cleared"

	update := BuildUpdateOpFromPrev(head, "bafyprev", "renamed.example.com")

	if got := update.VerificationMethods["atproto"]; got != "did:key:zUserRotatedToThis" {
		t.Errorf("verificationMethods[atproto] = %q; a rename must never revert a user's key rotation", got)
	}
	if got := update.Services["atproto_pds"].Endpoint; got != "https://elsewhere.example.com" {
		t.Errorf("services.atproto_pds.endpoint = %q; a rename must never revert a user's PDS move", got)
	}
	if update.Sig != "" {
		t.Errorf("sig = %q, want cleared", update.Sig)
	}

	// Deep copy: mutating the built op leaves the fetched head untouched.
	update.RotationKeys[0] = "did:key:zMutated"
	update.VerificationMethods["atproto"] = "did:key:zMutated"
	update.Services["atproto_pds"] = PlcService{Type: "x", Endpoint: "y"}
	if head.RotationKeys[0] != userDID ||
		head.VerificationMethods["atproto"] != "did:key:zUserRotatedToThis" ||
		head.Services["atproto_pds"].Endpoint != "https://elsewhere.example.com" {
		t.Error("BuildUpdateOpFromPrev aliased the previous op's slices/maps")
	}
}

// TestPrimaryHandle covers the alsoKnownAs read side, including the
// no-handle-asserted case a defensive caller must not misread as a match.
func TestPrimaryHandle(t *testing.T) {
	if got := PrimaryHandle(&PlcOperation{AlsoKnownAs: []string{"at://alice.example.com", "at://other"}}); got != "alice.example.com" {
		t.Errorf("PrimaryHandle = %q, want the first entry with at:// stripped", got)
	}
	if got := PrimaryHandle(&PlcOperation{}); got != "" {
		t.Errorf("PrimaryHandle(no alsoKnownAs) = %q, want empty", got)
	}
}
