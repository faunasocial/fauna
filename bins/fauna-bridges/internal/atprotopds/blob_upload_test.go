package atprotopds

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"sync"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
)

// fakeIngester records the three legs in the order they were called, which is
// what lets the ordering property be asserted rather than assumed.
type fakeIngester struct {
	mu    sync.Mutex
	calls []string

	// mediaRef is what the "nest" answers for stored bytes.
	mediaRef []byte
	// faunaCID is what the nest answers as the canonical Fauna spelling.
	faunaCID string

	storeErr  error
	recordErr error
	serveErr  error

	storedMIME  string
	storedBytes []byte
	mappedActor []byte
	mappedCID   string
	mappedRef   []byte
	servedDID   string
	servedCID   string
	servedFauna string
	servedMIME  string
	servedBytes []byte
}

func newFakeIngester() *fakeIngester {
	return &fakeIngester{
		mediaRef: bytes.Repeat([]byte{0xAB}, 32),
		faunaCID: "bafkqafauna32spellingfromthenest",
	}
}

func (f *fakeIngester) StoreInFaunaMedia(_ context.Context, mime string, data []byte) ([]byte, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, "store")
	if f.storeErr != nil {
		return nil, f.storeErr
	}
	f.storedMIME, f.storedBytes = mime, append([]byte(nil), data...)
	return f.mediaRef, nil
}

func (f *fakeIngester) RecordBlobMapping(_ context.Context, actorID []byte, cid string, mediaRef []byte) (string, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, "record")
	if f.recordErr != nil {
		return "", f.recordErr
	}
	f.mappedActor = append([]byte(nil), actorID...)
	f.mappedCID = cid
	f.mappedRef = append([]byte(nil), mediaRef...)
	return f.faunaCID, nil
}

func (f *fakeIngester) ServeBlobBytes(_ context.Context, did, cid, faunaCID, mime string, data []byte) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, "serve")
	if f.serveErr != nil {
		return f.serveErr
	}
	f.servedDID, f.servedCID, f.servedFauna, f.servedMIME = did, cid, faunaCID, mime
	f.servedBytes = append([]byte(nil), data...)
	return nil
}

func (f *fakeIngester) sequence() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.calls...)
}

// blobFixture is newFixture plus a wired BlobIngester.
func newBlobFixture(t *testing.T) (*fixture, *fakeIngester) {
	t.Helper()
	f := newFixture(t)
	b := newFakeIngester()
	f.server.EnableBlobUploads(b)
	return f, b
}

// pngBytes is the smallest byte prefix net/http's sniffer recognises as a PNG,
// padded so the body is not degenerate.
func pngBytes() []byte {
	return append([]byte("\x89PNG\r\n\x1a\n"), bytes.Repeat([]byte{0x11}, 64)...)
}

type uploadReply struct {
	Blob struct {
		Type string `json:"$type"`
		Ref  struct {
			Link string `json:"$link"`
		} `json:"ref"`
		MimeType string `json:"mimeType"`
		Size     int64  `json:"size"`
	} `json:"blob"`
}

func (f *fixture) uploadBlobCall(t *testing.T, token string, body []byte) *http.Response {
	t.Helper()
	return f.postBody(t, "com.atproto.repo.uploadBlob", token, body)
}

// The F2.4 headline: a session-authed uploadBlob answers a blob ref whose CID is
// the sha256 of the bytes, and the bytes are on both sides of the CID divide
// before the answer is written.
func TestUploadBlobAnswersARefBackedOnBothSides(t *testing.T) {
	f, b := newBlobFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	data := pngBytes()

	resp := f.uploadBlobCall(t, sess.AccessJwt, data)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("uploadBlob: %d", resp.StatusCode)
	}
	var got uploadReply
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatal(err)
	}

	wantCID, err := atprotorepo.BlobCIDForBytes(data)
	if err != nil {
		t.Fatal(err)
	}
	if got.Blob.Type != "blob" {
		t.Errorf("$type = %q, want blob", got.Blob.Type)
	}
	if got.Blob.Ref.Link != wantCID.String() {
		t.Errorf("ref.$link = %q, want the sha256 CID %q", got.Blob.Ref.Link, wantCID.String())
	}
	if got.Blob.MimeType != "image/png" {
		t.Errorf("mimeType = %q, want the SNIFFED image/png", got.Blob.MimeType)
	}
	if got.Blob.Size != int64(len(data)) {
		t.Errorf("size = %d, want %d", got.Blob.Size, len(data))
	}

	// Both sides hold the bytes, keyed the way each side names them.
	if !bytes.Equal(b.storedBytes, data) {
		t.Error("the Fauna media leg did not receive the bytes verbatim")
	}
	if b.mappedCID != wantCID.String() || !bytes.Equal(b.mappedRef, b.mediaRef) {
		t.Errorf("mapping = (%q, %x), want (%q, %x)",
			b.mappedCID, b.mappedRef, wantCID.String(), b.mediaRef)
	}
	if b.servedCID != wantCID.String() || !bytes.Equal(b.servedBytes, data) {
		t.Error("com.atproto.sync.getBlob would not serve the ref that was answered")
	}
	// The provenance key is the NEST's spelling, carried through untouched — a
	// Go-side re-derivation is exactly what this seam exists to prevent.
	if b.servedFauna != b.faunaCID {
		t.Errorf("served fauna cid = %q, want the nest-answered %q", b.servedFauna, b.faunaCID)
	}
}

// The mapping must name the authenticated ACCOUNT, not the bridge: the byte leg
// authenticates as the bridge's own service user and cannot know the account, so
// this leg is the only place the attribution can be right.
func TestUploadBlobAttributesTheMappingToTheAuthenticatedAccount(t *testing.T) {
	f, b := newBlobFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	if resp := f.uploadBlobCall(t, sess.AccessJwt, pngBytes()); resp.StatusCode != http.StatusOK {
		t.Fatalf("uploadBlob: %d", resp.StatusCode)
	}
	caller, err := f.server.VerifyAccess(nil, sess.AccessJwt)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(b.mappedActor, caller.ActorID) {
		t.Errorf("mapped actor = %x, want the caller's %x", b.mappedActor, caller.ActorID)
	}
	if b.servedDID != caller.DID {
		t.Errorf("served did = %q, want the caller's %q", b.servedDID, caller.DID)
	}
}

// The load-bearing ORDER: the bytes reach the Fauna media path and the mapping
// lands BEFORE the blob becomes servable. Bridge-store-first would leave bytes
// com.atproto.sync.getBlob serves with no Fauna media backing, so a record could
// reference bytes the round-trip cannot resolve.
func TestUploadBlobStoresInFaunaMediaBeforeMakingItServable(t *testing.T) {
	f, b := newBlobFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	if resp := f.uploadBlobCall(t, sess.AccessJwt, pngBytes()); resp.StatusCode != http.StatusOK {
		t.Fatalf("uploadBlob: %d", resp.StatusCode)
	}
	want := []string{"store", "record", "serve"}
	got := b.sequence()
	if fmt.Sprint(got) != fmt.Sprint(want) {
		t.Fatalf("leg order = %v, want %v", got, want)
	}
}

// A failure part-way must not make the blob servable, because the caller is being
// told the upload failed: a servable ref for a failed call is precisely the
// "answer names something that did not happen" failure the write path's own
// rulings reject.
func TestUploadBlobDoesNotServeTheBlobWhenAnEarlierLegFails(t *testing.T) {
	for _, tc := range []struct {
		name     string
		wire     func(*fakeIngester)
		wantLegs []string
	}{
		{"the fauna media leg fails", func(b *fakeIngester) { b.storeErr = errors.New("nest down") }, []string{"store"}},
		{"the mapping leg fails", func(b *fakeIngester) { b.recordErr = errors.New("ws-rpc down") }, []string{"store", "record"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f, b := newBlobFixture(t)
			_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
			tc.wire(b)

			resp := f.uploadBlobCall(t, sess.AccessJwt, pngBytes())
			if resp.StatusCode != http.StatusInternalServerError {
				t.Fatalf("status = %d, want 500", resp.StatusCode)
			}
			if fmt.Sprint(b.sequence()) != fmt.Sprint(tc.wantLegs) {
				t.Fatalf("legs = %v, want %v", b.sequence(), tc.wantLegs)
			}
			if b.servedCID != "" {
				t.Error("a failed upload left the blob servable")
			}
		})
	}
}

// R2 (account-data-plane.md § The ratified decisions): the type is sniffed from the BYTES, and bytes we cannot name are REFUSED —
// not accepted under an invented type, and not silently dropped the way the
// outbound projection drops an unpublishable attachment. Nothing may be written
// anywhere, on any leg.
func TestUploadBlobRefusesBytesItCannotName(t *testing.T) {
	f, b := newBlobFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	resp := f.uploadBlobCall(t, sess.AccessJwt, []byte("this is plainly not an image at all"))
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400", resp.StatusCode)
	}
	if len(b.sequence()) != 0 {
		t.Fatalf("a refused upload wrote something: %v", b.sequence())
	}
}

// The caller's Content-Type must never become the type the blob is served under:
// that value would be the Content-Type the nest answers on download, so trusting
// it lets a caller decide what its bytes are later claimed to be.
func TestUploadBlobIgnoresTheCallerDeclaredContentType(t *testing.T) {
	f, b := newBlobFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	data := pngBytes()

	req, _ := http.NewRequest(http.MethodPost,
		f.http.URL+"/xrpc/com.atproto.repo.uploadBlob", bytes.NewReader(data))
	req.Header.Set("Authorization", "Bearer "+sess.AccessJwt)
	// A lie the sniffer will contradict.
	req.Header.Set("Content-Type", "image/svg+xml")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("uploadBlob: %d", resp.StatusCode)
	}
	var got uploadReply
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatal(err)
	}
	if got.Blob.MimeType != "image/png" {
		t.Errorf("mimeType = %q, want the sniffed image/png, not the declared type", got.Blob.MimeType)
	}
	if b.storedMIME != "image/png" {
		t.Errorf("the sidecar would carry %q, want the sniffed image/png", b.storedMIME)
	}
}

// R6: the ceiling refuses without buffering the whole body, and it sits strictly
// below the nest's multipart cap so a blob can always fit beside its own sidecar.
func TestUploadBlobRefusesABodyOverTheCeiling(t *testing.T) {
	f, b := newBlobFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	oversize := append(pngBytes(), bytes.Repeat([]byte{0x22}, atprotorepo.MaxUploadBlobBytes)...)
	resp := f.uploadBlobCall(t, sess.AccessJwt, oversize)
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400", resp.StatusCode)
	}
	if len(b.sequence()) != 0 {
		t.Fatalf("an oversize upload wrote something: %v", b.sequence())
	}
}

// The coupling R6 exists for, asserted rather than trusted to a comment: the
// inbound ceiling must leave room for the sidecar part and the multipart framing
// under the nest's 10 MiB whole-body cap (blob_routes.rs BLOB_BODY_LIMIT).
func TestTheUploadCeilingLeavesRoomForTheSidecarUnderTheNestBodyCap(t *testing.T) {
	const nestMultipartBodyLimit = 10 * 1024 * 1024
	if atprotorepo.MaxUploadBlobBytes >= nestMultipartBodyLimit {
		t.Fatalf("upload ceiling %d must be below the nest's %d-byte multipart cap",
			atprotorepo.MaxUploadBlobBytes, nestMultipartBodyLimit)
	}
	if headroom := nestMultipartBodyLimit - atprotorepo.MaxUploadBlobBytes; headroom < 4096 {
		t.Fatalf("headroom %d is too tight for a sidecar part plus MIME framing", headroom)
	}
}

// An empty body is refused rather than stored: the nest's PublicPost verifier has
// a length floor of 1, so an empty blob would fail there anyway — refusing here
// is the good error message, and it keeps a zero-byte ref off the network.
func TestUploadBlobRefusesAnEmptyBody(t *testing.T) {
	f, b := newBlobFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	resp := f.uploadBlobCall(t, sess.AccessJwt, nil)
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400", resp.StatusCode)
	}
	if len(b.sequence()) != 0 {
		t.Fatalf("an empty upload wrote something: %v", b.sequence())
	}
}

// Fail-closed like every other seam: an unwired ingester refuses rather than
// accepting bytes it cannot persist.
func TestUploadBlobRefusesWithNoIngesterWired(t *testing.T) {
	f := newFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	resp := f.uploadBlobCall(t, sess.AccessJwt, pngBytes())
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want 404 MethodNotImplemented", resp.StatusCode)
	}
}

// The route is session-authed: an anonymous upload must not reach the ingester.
func TestUploadBlobRequiresASession(t *testing.T) {
	f, b := newBlobFixture(t)

	resp := f.uploadBlobCall(t, "", pngBytes())
	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("status = %d, want 401", resp.StatusCode)
	}
	if len(b.sequence()) != 0 {
		t.Fatalf("an unauthenticated upload wrote something: %v", b.sequence())
	}
}

// uploadBlob is in D6's served-locally bucket and is NOT Proxyable, so an
// attacker-supplied atproto-proxy header must not change where the bytes go: a
// forged header that forwarded the call would hand a user's image to a third
// party. Asserted behaviourally — the upload still lands on the local legs.
//
// (The endpoint-class name itself is pinned cross-language by
// TestEndpointClassNamesMatchTheModule in authz_test.go, beside the other four:
// a Go-only class addition would make D8's closed world deny every upload as
// though the token were bad — atproto-pds-full.md :230.)
func TestUploadBlobIgnoresAForgedProxyHeader(t *testing.T) {
	f, b := newBlobFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	data := pngBytes()

	req, _ := http.NewRequest(http.MethodPost,
		f.http.URL+"/xrpc/com.atproto.repo.uploadBlob", bytes.NewReader(data))
	req.Header.Set("Authorization", "Bearer "+sess.AccessJwt)
	req.Header.Set("atproto-proxy", "did:web:evil.example#atproto_pds")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("uploadBlob: %d", resp.StatusCode)
	}
	if !bytes.Equal(b.storedBytes, data) {
		t.Error("a forged atproto-proxy header diverted the upload away from the local legs")
	}
}
