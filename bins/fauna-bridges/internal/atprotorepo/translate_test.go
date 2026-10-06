package atprotorepo

import (
	"bytes"
	"context"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/repo"
	"github.com/bluesky-social/indigo/atproto/syntax"
)

// TestJSONRecordToDagCBOR converts a faceted post record (whose facet indices
// are integers — the dag-cbor no-floats tripwire) and proves the bytes are a
// valid atproto record: they round-trip through the funnel and indigo's loader
// byte-for-byte.
func TestJSONRecordToDagCBOR(t *testing.T) {
	recordJSON := `{
		"$type": "app.bsky.feed.post",
		"text": "hello @alice.example",
		"createdAt": "2026-01-01T00:00:00Z",
		"langs": ["en"],
		"facets": [{
			"index": {"byteStart": 6, "byteEnd": 20},
			"features": [{"$type": "app.bsky.richtext.facet#mention", "did": "did:web:alice.example"}]
		}]
	}`
	cbor, err := JSONRecordToDagCBOR(recordJSON)
	if err != nil {
		t.Fatalf("convert faceted record: %v", err)
	}
	if len(cbor) == 0 {
		t.Fatal("empty dag-cbor")
	}
	// Deterministic: the encoder is canonical.
	cbor2, err := JSONRecordToDagCBOR(recordJSON)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(cbor, cbor2) {
		t.Error("conversion is not deterministic")
	}

	// The bytes are a valid record: project them and reload through indigo.
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, _ := atcrypto.GeneratePrivateKeyK256()
	did := "did:web:alice.example"
	announced(t, st, did)
	rk := tids(1)[0]
	if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: rk, RecordCBOR: cbor, FaunaPostID: "aa",
	}}); err != nil {
		t.Fatalf("apply faceted record: %v", err)
	}
	carBytes, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	_, rr, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(carBytes))
	if err != nil {
		t.Fatalf("reload: %v", err)
	}
	raw, _, err := rr.GetRecordBytes(ctx, syntax.NSID(feedPost), syntax.RecordKey(rk))
	if err != nil {
		t.Fatalf("faceted record not retrievable: %v", err)
	}
	if !bytes.Equal(raw, cbor) {
		t.Error("record bytes were not preserved through the MST")
	}
}

// TestJSONRecordRejectsFloat: a non-integer number cannot be represented in
// dag-cbor and must be rejected, not silently emitted as a float.
func TestJSONRecordRejectsFloat(t *testing.T) {
	if _, err := JSONRecordToDagCBOR(`{"$type":"app.bsky.feed.post","n":1.5}`); err == nil {
		t.Error("expected an error for a non-integer number")
	}
}
