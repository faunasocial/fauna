package mta

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// newTestBytePlane spins an in-memory content-addressed chunk store over
// httptest and returns a byteplane.Client pointed at it, plus a cleanup. It
// mirrors the nest's /api/v1/chunks routes closely enough for the
// staged-envelope round trip: POST stores the body under its X-Content-Hash and
// 201s {"hash":...}; GET returns the stored bytes (or 404). The bearer is
// ignored — the store models only the content-addressed byte plane.
func newTestBytePlane(t *testing.T) (*byteplane.Client, func()) {
	t.Helper()
	var mu sync.Mutex
	store := map[string][]byte{}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/api/v1/chunks":
			hash := r.Header.Get("X-Content-Hash")
			body, _ := io.ReadAll(r.Body)
			mu.Lock()
			store[hash] = body
			mu.Unlock()
			w.WriteHeader(http.StatusCreated)
			_ = json.NewEncoder(w).Encode(map[string]string{"hash": hash})
		case r.Method == http.MethodGet && strings.HasPrefix(r.URL.Path, "/api/v1/chunks/"):
			hash := strings.TrimPrefix(r.URL.Path, "/api/v1/chunks/")
			mu.Lock()
			b, ok := store[hash]
			mu.Unlock()
			if !ok {
				w.WriteHeader(http.StatusNotFound)
				return
			}
			_, _ = w.Write(b)
		default:
			w.WriteHeader(http.StatusNotFound)
		}
	}))
	return byteplane.New(srv.URL, srv.Client()), srv.Close
}

// stageForTest AEAD-seals `plain`, splits the ciphertext, uploads the chunks to
// `plane`, and returns the wire reference the enqueue/fetch legs carry. It is
// the test-side mirror of what the MTA (mta/body_ref.go) and nest do when they
// stage an over-inline-budget outbound body — built from the shared FFI so the
// bytes match production exactly.
func stageForTest(t *testing.T, plane *byteplane.Client, plain []byte) *wsrpc.StagedBodyRef {
	t.Helper()
	seal := mailfauna.SealStagedBody(plain)
	chunks := mailfauna.SplitSealedMailBody(seal.Sealed)
	hashes := make([][]byte, 0, len(chunks))
	for _, c := range chunks {
		if err := plane.UploadChunk(context.Background(), "test-token", c.Bytes, hex.EncodeToString(c.Hash)); err != nil {
			t.Fatalf("upload staged chunk: %v", err)
		}
		hashes = append(hashes, c.Hash)
	}
	return &wsrpc.StagedBodyRef{
		ChunkHashes: hashes,
		TotalBytes:  uint64(len(seal.Sealed)),
		Key:         seal.Key,
	}
}

// TestOutboundWorkerResolvesStagedBody: an OutboundUnit whose body rode the
// bulk-byte plane under a one-shot AEAD envelope (RawMessage empty, StagedBody
// set) resolves via bodyFor to the byte-for-byte original plaintext.
func TestOutboundWorkerResolvesStagedBody(t *testing.T) {
	t.Parallel()
	plane, cleanup := newTestBytePlane(t)
	defer cleanup()

	raw := []byte("From: alice@example.com\r\nTo: bob@dest.test\r\n" +
		"Message-ID: <staged@example.com>\r\nSubject: staged\r\n\r\n" +
		strings.Repeat("the quick brown fox jumps over the lazy dog\r\n", 64))
	ref := stageForTest(t, plane, raw)

	w := &OutboundWorker{bytePlane: plane}
	u := wsrpc.OutboundUnit{
		ID:         42,
		Recipient:  "bob@dest.test",
		RawMessage: nil, // empty: the body rides by reference
		StagedBody: ref,
	}
	got, err := w.bodyFor(context.Background(), u)
	if err != nil {
		t.Fatalf("bodyFor(staged) unexpected error: %v", err)
	}
	if string(got) != string(raw) {
		t.Fatalf("staged body did not round-trip:\n got %q\nwant %q", got, raw)
	}
}

// TestOutboundWorkerStagedBodyCorruptKeyFailsClosed: a reference whose one-shot
// key is corrupt must fail the AEAD open — bodyFor returns an error, never a
// silent partial/empty body (fail-closed; deliverOne then reports a transient
// failure and nest re-stages with a fresh reference).
func TestOutboundWorkerStagedBodyCorruptKeyFailsClosed(t *testing.T) {
	t.Parallel()
	plane, cleanup := newTestBytePlane(t)
	defer cleanup()

	raw := []byte("From: alice@example.com\r\nSubject: staged\r\n\r\n" +
		strings.Repeat("payload ", 128))
	ref := stageForTest(t, plane, raw)
	// Flip one bit of the one-shot key: the AEAD tag no longer authenticates.
	ref.Key[0] ^= 0xFF

	w := &OutboundWorker{bytePlane: plane}
	u := wsrpc.OutboundUnit{Recipient: "bob@dest.test", StagedBody: ref}
	got, err := w.bodyFor(context.Background(), u)
	if err == nil {
		t.Fatalf("corrupt-key staged body must fail closed; got %d bytes and nil error", len(got))
	}
	if got != nil {
		t.Fatalf("failed resolve must return no body; got %q", got)
	}
}

// TestOutboundWorkerStagedBodyTotalMismatchFailsClosed: a reference whose
// declared total does not match the rejoined chunk bytes must fail before the
// AEAD is ever opened — the total is the cheap end-to-end check that catches a
// reference naming the wrong chunks, reordering them, or dropping one.
func TestOutboundWorkerStagedBodyTotalMismatchFailsClosed(t *testing.T) {
	t.Parallel()
	plane, cleanup := newTestBytePlane(t)
	defer cleanup()

	raw := []byte("From: alice@example.com\r\nSubject: staged\r\n\r\nhello world\r\n")
	ref := stageForTest(t, plane, raw)
	ref.TotalBytes++ // lie about the length

	w := &OutboundWorker{bytePlane: plane}
	u := wsrpc.OutboundUnit{Recipient: "bob@dest.test", StagedBody: ref}
	if _, err := w.bodyFor(context.Background(), u); err == nil {
		t.Fatal("declared-total mismatch must fail closed; got nil error")
	}
}

// TestOutboundWorkerStagedBodyNoPlaneFailsClosed: a staged unit with no
// byte-plane client wired cannot be resolved — bodyFor errors rather than
// shipping the empty RawMessage.
func TestOutboundWorkerStagedBodyNoPlaneFailsClosed(t *testing.T) {
	t.Parallel()
	w := &OutboundWorker{} // bytePlane nil
	u := wsrpc.OutboundUnit{
		Recipient:  "bob@dest.test",
		StagedBody: &wsrpc.StagedBodyRef{ChunkHashes: [][]byte{{0x01}}, TotalBytes: 1, Key: make([]byte, 32)},
	}
	if _, err := w.bodyFor(context.Background(), u); err == nil {
		t.Fatal("staged unit with no byte plane must fail closed; got nil error")
	}
}
