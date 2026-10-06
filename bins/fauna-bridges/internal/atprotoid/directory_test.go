package atprotoid

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
)

// signedTestGenesis builds + signs a genesis op and returns it with its
// derived DID and CID.
func signedTestGenesis(t *testing.T) (op *PlcOperation, did, cidStr, userDID, bridgeDID string) {
	t.Helper()
	mk := func() (atcrypto.PrivateKeyExportable, string) {
		k, err := atcrypto.GeneratePrivateKeyK256()
		if err != nil {
			t.Fatalf("GeneratePrivateKeyK256: %v", err)
		}
		d, err := DIDKeyForPrivate(k)
		if err != nil {
			t.Fatalf("DIDKeyForPrivate: %v", err)
		}
		return k, d
	}
	_, userDID = mk()
	bridgeKey, bridgeDIDKey := mk()
	_, signingDID := mk()
	op = BuildGenesisOp(userDID, bridgeDIDKey, signingDID, "alice.example.com", "https://example.com")
	if err := op.Sign(bridgeKey); err != nil {
		t.Fatalf("Sign: %v", err)
	}
	signed, err := op.SignedCBOR()
	if err != nil {
		t.Fatalf("SignedCBOR: %v", err)
	}
	c, err := GenesisCid(signed)
	if err != nil {
		t.Fatalf("GenesisCid: %v", err)
	}
	return op, DerivePlcDid(signed), c, userDID, bridgeDIDKey
}

// TestSubmitOperationGenesis drives SubmitOperation against a fake directory
// and asserts the wire contract: POST /{derived-did}, application/json, JSON
// body carrying rotationKeys in seniority order, an explicit null prev, and
// the sig.
func TestSubmitOperationGenesis(t *testing.T) {
	op, did, _, userDID, bridgeDID := signedTestGenesis(t)

	var gotPath, gotContentType string
	var gotBody []byte
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			t.Errorf("method = %s, want POST", r.Method)
		}
		gotPath = r.URL.Path
		gotContentType = r.Header.Get("Content-Type")
		gotBody, _ = io.ReadAll(r.Body)
		w.WriteHeader(http.StatusOK)
	}))
	defer srv.Close()

	if err := SubmitOperation(context.Background(), srv.Client(), srv.URL, did, op); err != nil {
		t.Fatalf("SubmitOperation: %v", err)
	}
	if gotPath != "/"+did {
		t.Errorf("path = %q, want /%s (genesis URL DID == derived DID)", gotPath, did)
	}
	if gotContentType != "application/json" {
		t.Errorf("content-type = %q, want application/json", gotContentType)
	}

	var body struct {
		Type         string            `json:"type"`
		RotationKeys []string          `json:"rotationKeys"`
		Prev         *string           `json:"prev"`
		Sig          string            `json:"sig"`
		Verification map[string]string `json:"verificationMethods"`
	}
	if err := json.Unmarshal(gotBody, &body); err != nil {
		t.Fatalf("body is not JSON: %v (%s)", err, gotBody)
	}
	if body.Type != "plc_operation" {
		t.Errorf("type = %q, want plc_operation", body.Type)
	}
	if len(body.RotationKeys) != 2 || body.RotationKeys[0] != userDID || body.RotationKeys[1] != bridgeDID {
		t.Errorf("rotationKeys = %v, want [user %q, bridge %q]", body.RotationKeys, userDID, bridgeDID)
	}
	if body.Prev != nil {
		t.Errorf("prev = %v, want explicit null", *body.Prev)
	}
	if body.Sig == "" {
		t.Error("sig missing from the submitted JSON")
	}
	// The explicit-null prev must be PRESENT in the raw JSON (spec: explicit
	// key), not omitted.
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(gotBody, &raw); err != nil {
		t.Fatalf("raw unmarshal: %v", err)
	}
	if _, ok := raw["prev"]; !ok {
		t.Error(`genesis JSON omitted "prev"; spec wants an explicit null`)
	}
}

// TestSubmitOperationUpdate covers the non-genesis path: prev rides as the
// prior CID string.
func TestSubmitOperationUpdate(t *testing.T) {
	genesis, did, prevCID, _, _ := signedTestGenesis(t)

	bridgeKey, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatalf("GeneratePrivateKeyK256: %v", err)
	}
	update := BuildUpdateOpFromPrev(genesis, prevCID, "renamed.example.com")
	if err := update.Sign(bridgeKey); err != nil {
		t.Fatalf("Sign: %v", err)
	}

	var gotBody []byte
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotBody, _ = io.ReadAll(r.Body)
		w.WriteHeader(http.StatusOK)
	}))
	defer srv.Close()

	if err := SubmitOperation(context.Background(), srv.Client(), srv.URL, did, update); err != nil {
		t.Fatalf("SubmitOperation(update): %v", err)
	}
	var body struct {
		Prev *string `json:"prev"`
	}
	if err := json.Unmarshal(gotBody, &body); err != nil {
		t.Fatalf("body is not JSON: %v", err)
	}
	if body.Prev == nil || *body.Prev != prevCID {
		t.Errorf("prev = %v, want %q", body.Prev, prevCID)
	}
}

// TestSubmitOperationNon2xx surfaces the directory's rejection body text.
func TestSubmitOperationNon2xx(t *testing.T) {
	op, did, _, _, _ := signedTestGenesis(t)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "invalid operation: recovery window", http.StatusBadRequest)
	}))
	defer srv.Close()

	err := SubmitOperation(context.Background(), srv.Client(), srv.URL, did, op)
	if err == nil {
		t.Fatal("SubmitOperation succeeded on a 400; want error")
	}
	for _, want := range []string{"400", "recovery window"} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("error %q does not carry %q", err, want)
		}
	}
}

// TestSubmitOperationRefusesUnsigned locks the unsigned-op guard.
func TestSubmitOperationRefusesUnsigned(t *testing.T) {
	op := BuildGenesisOp("did:key:zU", "did:key:zB", "did:key:zS", "a.example.com", "https://example.com")
	if err := SubmitOperation(context.Background(), http.DefaultClient, "http://127.0.0.1:1", "did:plc:x", op); err == nil {
		t.Fatal("SubmitOperation accepted an unsigned op")
	}
}

// TestPLCDirectoryBaseURL locks the production default, which both flavors
// share. The test-only env seam's parse is pinned in directory_seam_test.go
// (e2e flavor) and its inertness in directory_seam_absent_test.go (production).
func TestPLCDirectoryBaseURL(t *testing.T) {
	// Spelled out: the production package carries no constant for the seam.
	t.Setenv("FAUNA_ATPROTO_PLC_DIRECTORY_URL", "")
	if got := PLCDirectoryBaseURL(); got != DefaultPLCDirectoryURL {
		t.Errorf("default = %q, want %q", got, DefaultPLCDirectoryURL)
	}
}

// auditServer serves a canned `/{did}/log/audit` response and reports the path
// it was asked for, so the tests pin the URL shape as well as the parse.
func auditServer(t *testing.T, body string, status int) (*httptest.Server, *string) {
	t.Helper()
	var seen string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seen = r.URL.Path
		if status != http.StatusOK {
			http.Error(w, "nope", status)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, body)
	}))
	t.Cleanup(srv.Close)
	return srv, &seen
}

// TestFetchLastOpReturnsHeadAndCID: the standing head of the log is returned
// with its CID — the two things a rename needs (state to delta from, prev link).
func TestFetchLastOpReturnsHeadAndCID(t *testing.T) {
	body := `[
	  {"cid":"bafygenesis","nullified":false,"operation":{"type":"plc_operation","rotationKeys":["did:key:zUser","did:key:zBridge"],"verificationMethods":{"atproto":"did:key:zSign"},"alsoKnownAs":["at://alice.example.com"],"services":{"atproto_pds":{"type":"AtprotoPersonalDataServer","endpoint":"https://example.com"}},"prev":null,"sig":"sig0"}},
	  {"cid":"bafyupdate","nullified":false,"operation":{"type":"plc_operation","rotationKeys":["did:key:zUser","did:key:zBridge"],"verificationMethods":{"atproto":"did:key:zUserRotated"},"alsoKnownAs":["at://alice.example.com"],"services":{"atproto_pds":{"type":"AtprotoPersonalDataServer","endpoint":"https://example.com"}},"prev":"bafygenesis","sig":"sig1"}}
	]`
	srv, seen := auditServer(t, body, http.StatusOK)
	op, cid, err := FetchLastOp(context.Background(), srv.Client(), srv.URL, "did:plc:abc")
	if err != nil {
		t.Fatalf("FetchLastOp: %v", err)
	}
	if *seen != "/did:plc:abc/log/audit" {
		t.Errorf("requested path = %q, want /{did}/log/audit", *seen)
	}
	if cid != "bafyupdate" {
		t.Errorf("cid = %q, want the LAST entry's cid (the prev link)", cid)
	}
	if op.VerificationMethods["atproto"] != "did:key:zUserRotated" {
		t.Errorf("returned the wrong op: verificationMethods[atproto] = %q", op.VerificationMethods["atproto"])
	}
	if got := PrimaryHandle(op); got != "alice.example.com" {
		t.Errorf("PrimaryHandle = %q", got)
	}
}

// TestFetchLastOpSkipsNullified: an op a senior rotation key contested inside
// PLC's 72h window is no longer in the chain, so chaining prev to it would be
// rejected by the directory. Walk back to the last standing entry instead.
func TestFetchLastOpSkipsNullified(t *testing.T) {
	body := `[
	  {"cid":"bafygenesis","nullified":false,"operation":{"type":"plc_operation","rotationKeys":["did:key:zUser"],"verificationMethods":{},"alsoKnownAs":["at://alice.example.com"],"services":{},"prev":null,"sig":"s"}},
	  {"cid":"bafynullified","nullified":true,"operation":{"type":"plc_operation","rotationKeys":["did:key:zAttacker"],"verificationMethods":{},"alsoKnownAs":["at://evil.example.com"],"services":{},"prev":"bafygenesis","sig":"s"}}
	]`
	srv, _ := auditServer(t, body, http.StatusOK)
	op, cid, err := FetchLastOp(context.Background(), srv.Client(), srv.URL, "did:plc:abc")
	if err != nil {
		t.Fatalf("FetchLastOp: %v", err)
	}
	if cid != "bafygenesis" {
		t.Errorf("cid = %q, want the last NON-nullified entry", cid)
	}
	if got := PrimaryHandle(op); got != "alice.example.com" {
		t.Errorf("PrimaryHandle = %q; a nullified op must not be treated as current state", got)
	}
}

// TestFetchLastOpRefusesLegacyCreateOp: this bridge only manages DIDs it minted
// (always `plc_operation`), so a legacy v0 `create` head means the DID is not
// ours to update — refuse loudly rather than coerce it into our shape.
func TestFetchLastOpRefusesLegacyCreateOp(t *testing.T) {
	body := `[{"cid":"bafylegacy","nullified":false,"operation":{"type":"create","signingKey":"did:key:zSign","handle":"alice.example.com","service":"https://example.com","prev":null,"sig":"s"}}]`
	srv, _ := auditServer(t, body, http.StatusOK)
	if _, _, err := FetchLastOp(context.Background(), srv.Client(), srv.URL, "did:plc:abc"); err == nil {
		t.Fatal("FetchLastOp accepted a legacy create op; want a refusal")
	} else if !strings.Contains(err.Error(), "create") {
		t.Errorf("error should name the offending op type, got: %v", err)
	}
}

// TestFetchLastOpErrors: an empty log and a non-2xx both fail rather than
// silently yielding a zero op a caller could chain from.
func TestFetchLastOpErrors(t *testing.T) {
	srv, _ := auditServer(t, `[]`, http.StatusOK)
	if _, _, err := FetchLastOp(context.Background(), srv.Client(), srv.URL, "did:plc:abc"); err == nil {
		t.Error("empty audit log should error")
	}
	bad, _ := auditServer(t, ``, http.StatusNotFound)
	if _, _, err := FetchLastOp(context.Background(), bad.Client(), bad.URL, "did:plc:abc"); err == nil {
		t.Error("404 audit log should error")
	}
}

// TestFetchLastOpRoundTripsOurOwnGenesis pins the parse against a REAL op this
// package produced (not just hand-written JSON): the submit body shape and the
// audit-log entry shape are the same JSON, so a field-name drift on either side
// is caught here.
func TestFetchLastOpRoundTripsOurOwnGenesis(t *testing.T) {
	genesis, did, cidStr, userDID, _ := signedTestGenesis(t)
	opJSON, err := json.Marshal(genesis)
	if err != nil {
		t.Fatalf("marshal genesis: %v", err)
	}
	body := `[{"cid":"` + cidStr + `","nullified":false,"operation":` + string(opJSON) + `}]`
	srv, _ := auditServer(t, body, http.StatusOK)

	got, gotCID, err := FetchLastOp(context.Background(), srv.Client(), srv.URL, did)
	if err != nil {
		t.Fatalf("FetchLastOp: %v", err)
	}
	if gotCID != cidStr {
		t.Errorf("cid = %q, want %q", gotCID, cidStr)
	}
	if got.RotationKeys[0] != userDID {
		t.Errorf("rotationKeys[0] = %q, want the user's senior key round-tripped", got.RotationKeys[0])
	}
	if got.Sig != genesis.Sig {
		t.Errorf("sig did not round-trip: %q vs %q", got.Sig, genesis.Sig)
	}
	if got.Prev != nil {
		t.Errorf("genesis prev = %v, want nil", got.Prev)
	}
}
