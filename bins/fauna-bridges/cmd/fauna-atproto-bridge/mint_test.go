// Tests for the S2 DID mint loop (mint.go): a fake nest Caller serves the
// three fauna.bridges.atproto.* methods, a fake unseal stands in for the FFI
// HPKE-Open, and an httptest server plays the PLC directory — so the whole
// pending→minted flow (seniority ordering, signing, DID/CID derivation,
// directory submit, record report-back, scalar zeroization) runs headless.
package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

func discardLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}

// testScalar derives a deterministic 32-byte K-256 scalar per label.
func testScalar(label string) []byte {
	h := sha256.Sum256([]byte("fauna atproto mint test scalar: " + label))
	return h[:]
}

// didKeyForScalar derives the did:key pubkey string of a raw scalar.
func didKeyForScalar(t *testing.T, scalar []byte) string {
	t.Helper()
	key, err := atprotoid.PrivateKeyFromK256Scalar(scalar)
	if err != nil {
		t.Fatalf("PrivateKeyFromK256Scalar: %v", err)
	}
	didKey, err := atprotoid.DIDKeyForPrivate(key)
	if err != nil {
		t.Fatalf("DIDKeyForPrivate: %v", err)
	}
	return didKey
}

// recordedMint is one record_minted_identity call the fake nest saw.
type recordedMint struct {
	ActorID    []byte  `cbor:"actor_id"`
	DID        string  `cbor:"did"`
	GenesisCID *string `cbor:"genesis_cid"`
}

// fakeNest serves the three atproto methods off canned data.
type fakeNest struct {
	mu           sync.Mutex
	identities   []wsrpc.AtprotoIdentityView
	blob         []byte
	signingPub   string
	rotationPub  string
	keyBlobCalls int
	recorded     []recordedMint
}

func (f *fakeNest) Call(_ context.Context, method string, body, reply any) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	reencode := func(v any) error {
		b, err := cbor.Marshal(v)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(b, reply)
	}
	decodeBody := func(into any) error {
		b, err := cbor.Marshal(body)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(b, into)
	}
	switch method {
	case wsrpc.MethodAtprotoFetchIdentities:
		return reencode(map[string]any{"identities": f.identities})
	case wsrpc.MethodAtprotoFetchIdentityKeyBlob:
		f.keyBlobCalls++
		var req struct {
			ActorID []byte `cbor:"actor_id"`
		}
		if err := decodeBody(&req); err != nil {
			return err
		}
		return reencode(map[string]any{
			"blob":                        f.blob,
			"signing_pub_did_key":         f.signingPub,
			"bridge_rotation_pub_did_key": f.rotationPub,
		})
	case wsrpc.MethodAtprotoRecordMintedIdentity:
		var req recordedMint
		if err := decodeBody(&req); err != nil {
			return err
		}
		f.recorded = append(f.recorded, req)
		return reencode(map[string]any{})
	default:
		return fmt.Errorf("fakeNest: unexpected method %q", method)
	}
}

// fakeTXTResolver returns canned TXT records.
type fakeTXTResolver struct{ records map[string][]string }

func (f *fakeTXTResolver) LookupTXT(_ context.Context, name string) ([]string, error) {
	return f.records[name], nil
}

// mintFixture builds the whole fake world for one pending identity.
type mintFixture struct {
	nest          *fakeNest
	deps          *mintDeps
	directory     *httptest.Server
	dirPosts      *[]dirPost
	userDIDKey    string
	bridgeDIDKey  string
	signingDIDKey string
	// bundles returned by the fake unseal, for zeroization asserts.
	unsealed []*mailfauna.AtprotoIdentityKeyBundle
	// expected is every {signing, rotation} published-key pair the unseal was
	// handed — the binding the shared Rust enforces in production.
	expected [][2]string
}

type dirPost struct {
	path string
	body []byte
}

func newMintFixture(t *testing.T, identities []wsrpc.AtprotoIdentityView) *mintFixture {
	t.Helper()
	fx := &mintFixture{}
	fx.userDIDKey = didKeyForScalar(t, testScalar("user-rotation"))
	fx.bridgeDIDKey = didKeyForScalar(t, testScalar("bridge-rotation"))
	fx.signingDIDKey = didKeyForScalar(t, testScalar("signing"))

	posts := []dirPost{}
	fx.dirPosts = &posts
	fx.directory = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost {
			b, _ := io.ReadAll(r.Body)
			posts = append(posts, dirPost{path: r.URL.Path, body: b})
		}
		// GETs (the resolvability self-check) also succeed.
		w.WriteHeader(http.StatusOK)
	}))
	t.Cleanup(fx.directory.Close)

	fx.nest = &fakeNest{
		identities:  identities,
		blob:        []byte("sealed-blob-opaque-to-the-test"),
		signingPub:  fx.signingDIDKey,
		rotationPub: fx.bridgeDIDKey,
	}

	actorID := bytes.Repeat([]byte{0x11}, 32)
	fx.deps = &mintDeps{
		x25519Secret: bytes.Repeat([]byte{0x42}, 32),
		unseal: func(blobBytes, secret []byte, wantSigning, wantRotation string) (*mailfauna.AtprotoIdentityKeyBundle, error) {
			if !bytes.Equal(blobBytes, fx.nest.blob) {
				return nil, fmt.Errorf("unseal got unexpected blob")
			}
			// The fake stands in for the shared Rust, so it keeps the Rust's
			// contract: the expectation is the fetch reply's two published keys,
			// and a blob whose keys are not those is refused.
			fx.expected = append(fx.expected, [2]string{wantSigning, wantRotation})
			if wantSigning != fx.signingDIDKey || wantRotation != fx.bridgeDIDKey {
				return nil, fmt.Errorf("unsealed identity keys are not the identity's published keys")
			}
			if len(secret) != 32 {
				return nil, fmt.Errorf("unseal got %d-byte secret, want 32", len(secret))
			}
			// Fresh copies per call: the mint loop zeroizes in place.
			b := &mailfauna.AtprotoIdentityKeyBundle{
				ActorId:           append([]byte(nil), actorID...),
				SigningPriv:       testScalar("signing"),
				SigningCurve:      "k256",
				SigningPubDidKey:  fx.signingDIDKey,
				RotationPriv:      testScalar("bridge-rotation"),
				RotationCurve:     "k256",
				RotationPubDidKey: fx.bridgeDIDKey,
			}
			fx.unsealed = append(fx.unsealed, b)
			return b, nil
		},
		directoryBaseURL: fx.directory.URL,
		httpClient:       fx.directory.Client(),
		resolver:         &fakeTXTResolver{records: map[string][]string{}},
	}
	return fx
}

func pendingPlcIdentity(handle, userDIDKey string) wsrpc.AtprotoIdentityView {
	return wsrpc.AtprotoIdentityView{
		ActorID:               bytes.Repeat([]byte{0x11}, 32),
		Handle:                handle,
		Method:                "plc",
		Status:                "pending",
		UserRotationPubDIDKey: userDIDKey,
		PDSEndpoint:           "https://example.com",
	}
}

// TestMintPassPlc drives the full did:plc mint: the directory receives the
// signed genesis op at POST /{derived-did} with the USER rotation key at
// rotationKeys[0] and a signature verifying against the BRIDGE rotation
// pubkey; nest gets the same DID + genesis CID recorded; the unsealed scalars
// are zeroized afterward.
func TestMintPassPlc(t *testing.T) {
	fx := newMintFixture(t, []wsrpc.AtprotoIdentityView{
		pendingPlcIdentity("alice.example.com", "PLACEHOLDER"),
	})
	fx.nest.identities[0].UserRotationPubDIDKey = fx.userDIDKey

	runMintPass(context.Background(), fx.nest, fx.deps, discardLogger())

	// ── directory saw exactly one POST ──
	if len(*fx.dirPosts) != 1 {
		t.Fatalf("directory saw %d POSTs, want 1", len(*fx.dirPosts))
	}
	post := (*fx.dirPosts)[0]

	var op atprotoid.PlcOperation
	if err := json.Unmarshal(post.body, &op); err != nil {
		t.Fatalf("submitted body is not a JSON plc op: %v", err)
	}
	if op.Type != "plc_operation" {
		t.Errorf("type = %q", op.Type)
	}
	if len(op.RotationKeys) != 2 || op.RotationKeys[0] != fx.userDIDKey || op.RotationKeys[1] != fx.bridgeDIDKey {
		t.Errorf("rotationKeys = %v, want [user %q, bridge %q] (seniority invariant)",
			op.RotationKeys, fx.userDIDKey, fx.bridgeDIDKey)
	}
	if op.Prev != nil {
		t.Errorf("genesis prev = %v, want null", *op.Prev)
	}
	if op.VerificationMethods["atproto"] != fx.signingDIDKey {
		t.Errorf("verificationMethods.atproto = %q, want %q", op.VerificationMethods["atproto"], fx.signingDIDKey)
	}
	if len(op.AlsoKnownAs) != 1 || op.AlsoKnownAs[0] != "at://alice.example.com" {
		t.Errorf("alsoKnownAs = %v", op.AlsoKnownAs)
	}
	if svc := op.Services["atproto_pds"]; svc.Type != "AtprotoPersonalDataServer" || svc.Endpoint != "https://example.com" {
		t.Errorf("services.atproto_pds = %+v", svc)
	}

	// The submitted signature verifies against the bridge rotation PUBLIC key
	// (and NOT the user key — the user contributes only a pubkey at mint).
	bridgePub, err := atprotoid.ParsePublicDIDKey(fx.bridgeDIDKey)
	if err != nil {
		t.Fatalf("ParsePublicDIDKey: %v", err)
	}
	if err := op.VerifySig(bridgePub); err != nil {
		t.Errorf("submitted op does not verify against the bridge rotation pubkey: %v", err)
	}
	userPub, err := atprotoid.ParsePublicDIDKey(fx.userDIDKey)
	if err != nil {
		t.Fatalf("ParsePublicDIDKey(user): %v", err)
	}
	if err := op.VerifySig(userPub); err == nil {
		t.Error("submitted op verifies against the user key; the bridge must have signed")
	}

	// The URL DID equals the DID re-derived from the submitted op — the
	// directory's genesis special case.
	signed, err := op.SignedCBOR()
	if err != nil {
		t.Fatalf("SignedCBOR(decoded op): %v", err)
	}
	wantDID := atprotoid.DerivePlcDid(signed)
	if post.path != "/"+wantDID {
		t.Errorf("directory path = %q, want /%s", post.path, wantDID)
	}

	// ── nest record matches ──
	if len(fx.nest.recorded) != 1 {
		t.Fatalf("record_minted_identity called %d times, want 1", len(fx.nest.recorded))
	}
	rec := fx.nest.recorded[0]
	if rec.DID != wantDID {
		t.Errorf("recorded DID = %q, want %q", rec.DID, wantDID)
	}
	wantCID, err := atprotoid.GenesisCid(signed)
	if err != nil {
		t.Fatalf("GenesisCid: %v", err)
	}
	if rec.GenesisCID == nil || *rec.GenesisCID != wantCID {
		t.Errorf("recorded genesis_cid = %v, want %q", rec.GenesisCID, wantCID)
	}
	if !bytes.Equal(rec.ActorID, fx.nest.identities[0].ActorID) {
		t.Error("recorded actor_id mismatch")
	}

	// ── the unsealed scalars were zeroized in place ──
	if len(fx.unsealed) != 1 {
		t.Fatalf("unseal called %d times, want 1", len(fx.unsealed))
	}
	zero := make([]byte, 32)
	if !bytes.Equal(fx.unsealed[0].RotationPriv, zero) {
		t.Error("rotation scalar not zeroized after mint")
	}
	if !bytes.Equal(fx.unsealed[0].SigningPriv, zero) {
		t.Error("signing scalar not zeroized after mint")
	}
}

// TestMintPassWeb drives the did:web path: no directory call, no key blob
// fetch — just the constructed DID recorded with a null genesis CID.
func TestMintPassWeb(t *testing.T) {
	fx := newMintFixture(t, []wsrpc.AtprotoIdentityView{{
		ActorID:     bytes.Repeat([]byte{0x22}, 32),
		Handle:      "bob.example.com",
		Method:      "web",
		Status:      "pending",
		PDSEndpoint: "https://example.com",
	}})

	runMintPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if len(*fx.dirPosts) != 0 {
		t.Errorf("did:web mint POSTed to the directory %d times, want 0", len(*fx.dirPosts))
	}
	if fx.nest.keyBlobCalls != 0 {
		t.Errorf("did:web mint fetched the key blob %d times, want 0", fx.nest.keyBlobCalls)
	}
	if len(fx.nest.recorded) != 1 {
		t.Fatalf("record_minted_identity called %d times, want 1", len(fx.nest.recorded))
	}
	rec := fx.nest.recorded[0]
	if rec.DID != "did:web:bob.example.com" {
		t.Errorf("recorded DID = %q, want did:web:bob.example.com", rec.DID)
	}
	if rec.GenesisCID != nil {
		t.Errorf("recorded genesis_cid = %q, want null", *rec.GenesisCID)
	}
}

// TestMintPassSkipsNonPending locks the idempotence guard: active rows are
// never re-minted.
func TestMintPassSkipsNonPending(t *testing.T) {
	did := "did:plc:aaaabbbbccccddddeeeeffff"
	active := pendingPlcIdentity("alice.example.com", "did:key:zUser")
	active.Status = "active"
	active.DID = &did
	fx := newMintFixture(t, []wsrpc.AtprotoIdentityView{active})

	runMintPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if len(fx.nest.recorded) != 0 || len(*fx.dirPosts) != 0 || fx.nest.keyBlobCalls != 0 {
		t.Errorf("active identity was touched: recorded=%d posts=%d blobCalls=%d",
			len(fx.nest.recorded), len(*fx.dirPosts), fx.nest.keyBlobCalls)
	}
}

// TestMintPassPlcWithoutUserKeyRefuses locks the custody invariant's guard: a
// pending did:plc row with no user rotation pubkey is NOT minted (the bridge
// alone must never hold every rotation key), and the sweep continues to other
// identities.
func TestMintPassPlcWithoutUserKeyRefuses(t *testing.T) {
	fx := newMintFixture(t, []wsrpc.AtprotoIdentityView{
		pendingPlcIdentity("alice.example.com", ""), // no user key yet
		{
			ActorID:     bytes.Repeat([]byte{0x22}, 32),
			Handle:      "bob.example.com",
			Method:      "web",
			Status:      "pending",
			PDSEndpoint: "https://example.com",
		},
	})

	runMintPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if len(*fx.dirPosts) != 0 {
		t.Errorf("keyless plc identity reached the directory (%d POSTs)", len(*fx.dirPosts))
	}
	if len(fx.nest.recorded) != 1 || fx.nest.recorded[0].DID != "did:web:bob.example.com" {
		t.Errorf("sweep did not continue past the failing identity: recorded=%+v", fx.nest.recorded)
	}
}

// TestMintPassDirectoryRejection: a directory non-2xx means NO record — the
// identity stays pending and the next poll retries.
func TestMintPassDirectoryRejection(t *testing.T) {
	fx := newMintFixture(t, []wsrpc.AtprotoIdentityView{
		pendingPlcIdentity("alice.example.com", "PLACEHOLDER"),
	})
	fx.nest.identities[0].UserRotationPubDIDKey = fx.userDIDKey
	rejecting := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "nope", http.StatusBadRequest)
	}))
	defer rejecting.Close()
	fx.deps.directoryBaseURL = rejecting.URL
	fx.deps.httpClient = rejecting.Client()

	runMintPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if len(fx.nest.recorded) != 0 {
		t.Errorf("directory rejection still recorded a mint: %+v", fx.nest.recorded)
	}
}

// TestMintPassRefusesAnotherIdentitysBlob: genesis is where a wrong blob does
// lasting harm — its keys are published into a brand-new DID document. The
// blob the nest serves here holds some OTHER identity's keys (the identity row
// publishes a different pair), so the unseal refuses and nothing may reach the
// directory or be recorded. It also pins the wiring: the expectation handed to
// the unseal is exactly the fetch reply's two published keys.
func TestMintPassRefusesAnotherIdentitysBlob(t *testing.T) {
	fx := newMintFixture(t, []wsrpc.AtprotoIdentityView{
		pendingPlcIdentity("alice.example.com", "PLACEHOLDER"),
	})
	fx.nest.identities[0].UserRotationPubDIDKey = fx.userDIDKey
	fx.nest.signingPub = "did:key:zQ3sTHISIDENTITYsigning"
	fx.nest.rotationPub = "did:key:zQ3sTHISIDENTITYrotation"

	runMintPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if len(fx.expected) != 1 ||
		fx.expected[0] != [2]string{"did:key:zQ3sTHISIDENTITYsigning", "did:key:zQ3sTHISIDENTITYrotation"} {
		t.Fatalf("unseal expectation = %v, want the fetch reply's published keys", fx.expected)
	}
	if len(*fx.dirPosts) != 0 || len(fx.nest.recorded) != 0 {
		t.Errorf("another identity's keys were minted into this DID: posts=%d recorded=%d",
			len(*fx.dirPosts), len(fx.nest.recorded))
	}
}

// TestMintPassSealDrift: an unsealed rotation scalar whose pubkey differs
// from what nest recorded must refuse to sign (the signature would never
// verify ecosystem-side).
func TestMintPassSealDrift(t *testing.T) {
	fx := newMintFixture(t, []wsrpc.AtprotoIdentityView{
		pendingPlcIdentity("alice.example.com", "PLACEHOLDER"),
	})
	fx.nest.identities[0].UserRotationPubDIDKey = fx.userDIDKey
	baseUnseal := fx.deps.unseal
	fx.deps.unseal = func(blobBytes, secret []byte, wantSigning, wantRotation string) (*mailfauna.AtprotoIdentityKeyBundle, error) {
		b, err := baseUnseal(blobBytes, secret, wantSigning, wantRotation)
		if err != nil {
			return nil, err
		}
		b.RotationPriv = testScalar("some-other-scalar") // drifts from RotationPubDidKey
		return b, nil
	}

	runMintPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if len(*fx.dirPosts) != 0 || len(fx.nest.recorded) != 0 {
		t.Errorf("seal-drifted identity was minted anyway: posts=%d recorded=%d",
			len(*fx.dirPosts), len(fx.nest.recorded))
	}
}
