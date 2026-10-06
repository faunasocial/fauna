package main

import (
	"context"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
)

// funnelRepoWriter bridges two signer types: RepoSignerSource hands back the
// STRUCTURAL interface (atprotopds.RepoSigner, HashAndSign only — which is what
// keeps indigo out of that package), while the funnel signs a commit block and
// needs indigo's concrete key. The narrowing is a type assertion, so nothing
// but a test can prove the production key actually satisfies it: get it wrong
// and every external write fails at RUNTIME with "not an indigo private key",
// with no compile error anywhere.
func TestProductionRepoSignerNarrowsToTheFunnelsSignerType(t *testing.T) {
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	// The exact hop repoSignerForActor's result makes on its way to the funnel.
	var asSource atprotopds.RepoSigner = key
	if _, ok := asSource.(atprotorepo.Signer); !ok {
		t.Fatal("the production repo signing key does not satisfy atprotorepo.Signer; " +
			"every external write would fail at runtime")
	}
}

// A source that cannot produce a funnel-usable key must refuse, not commit an
// unsigned or wrongly-signed repo.
func TestRepoWriterRefusesANonIndigoSigner(t *testing.T) {
	w := newFunnelRepoWriter(nil, nil, &countingSignerSource{signer: nopSigner{}}, nil, nil)
	if _, err := w.Commit(context.Background(), make([]byte, 32), "did:fauna:x", nil); err == nil {
		t.Fatal("a signer the funnel cannot use must refuse the commit")
	}
}

// RenderProjectedRecord's two refusals, both of which must be ERRORS rather than
// an empty or `{}` record: the write path commits whatever this answers, so a
// silent empty answer would replace the account's real profile record with a
// blank one and answer a cid naming it (F2.4 slice 3).
//
// Only a test can catch the unknown-collection case: the nest decides which
// collections it asserts `reproject_record` for, so a future projection-owned
// collection reaches this switch with nothing failing at compile time.
func TestRenderProjectedRecordRefusesWhatItCannotRender(t *testing.T) {
	ctx := context.Background()
	// No projector wired, but a collection the projection does own: the nil
	// guard must fire rather than panic — the write path's error path, not a
	// crashed bridge.
	w := newFunnelRepoWriter(nil, nil, nil, nil, nil)
	if _, err := w.RenderProjectedRecord(ctx, make([]byte, 32), "did:plc:x", atprotorepo.NsidProfile); err == nil {
		t.Error("an unwired projector must refuse, not answer an empty record")
	}
	// A collection the projection owns no record in. Answering the caller's
	// bytes here would silently defeat the signal; answering "" would commit a
	// blank record.
	if _, err := w.RenderProjectedRecord(ctx, make([]byte, 32), "did:plc:x", "app.bsky.feed.post"); err == nil {
		t.Error("a collection the projection does not own must refuse loudly")
	}
}

// Two callers must never be handed the same record key.
func TestNextRkeyIsUnique(t *testing.T) {
	w := newFunnelRepoWriter(nil, nil, nil, nil, nil)
	seen := map[string]bool{}
	for i := 0; i < 64; i++ {
		k := w.NextRkey()
		if k == "" {
			t.Fatal("empty rkey")
		}
		if seen[k] {
			t.Fatalf("rkey %q handed out twice", k)
		}
		seen[k] = true
	}
}

// **The cross-binary blob-ref pin.** The Go encoder and the Rust walk must
// AGREE about which blobs a record names — not each match a fixture literal
// (the F2.2 slice-4d rule). Nothing else can prove it: the encoder is indigo's
// atdata codec here, the walk is shared Rust reached over the FFI, and every
// unit test on either side builds its own fixture in its own in-memory shape.
//
// They did NOT agree until 2026-07-30, and the disagreement was invisible and
// expensive. atdata stores a blob's `ref` as the literal map
// `{"$link": "<cid>"}`, while the walk understood only a dag-cbor tag-42 link —
// so the walk returned NOTHING for every record an external app ever wrote.
// The nest stamps `atproto_blobs.referenced_at` from this walk; an unstamped
// row is swept after 7 days and the box GC then reclaims the bytes, so every
// externally uploaded image was scheduled for deletion. The `#commit` frame's
// `blobs` field was always empty for the same reason. Both sides' unit tests
// were green throughout.
//
// So this test encodes the record the way production does and asks the real
// walk what it sees. A future codec bump that changes the stored link shape
// lands here as a red test rather than as silent data loss.
func TestTheRustBlobWalkSeesTheRefsTheGoEncoderReallyWrites(t *testing.T) {
	const cid = "bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4"
	for _, tc := range []struct {
		name string
		json string
	}{
		{
			// A profile picture, as an external app echoes it back from
			// getRecord — the inbound-picture path.
			name: "profile avatar",
			json: `{"$type":"app.bsky.actor.profile","displayName":"Ada",` +
				`"avatar":{"$type":"blob","ref":{"$link":"` + cid + `"},` +
				`"mimeType":"image/png","size":24}}`,
		},
		{
			// A post image, as an app echoes uploadBlob's own reply — the
			// path whose stamp the GC depends on.
			name: "post embed image",
			json: `{"$type":"app.bsky.feed.post","text":"hi",` +
				`"createdAt":"2026-07-30T10:00:00.000Z",` +
				`"embed":{"$type":"app.bsky.embed.images","images":[{"alt":"",` +
				`"image":{"$type":"blob","ref":{"$link":"` + cid + `"},` +
				`"mimeType":"image/png","size":24}}]}}`,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			recordCBOR, err := atprotorepo.JSONRecordToDagCBOR(tc.json)
			if err != nil {
				t.Fatalf("encode: %v", err)
			}
			w := &funnelRepoWriter{}
			got := w.ExtractBlobRefs(recordCBOR)
			if len(got) != 1 || got[0] != cid {
				t.Fatalf("the walk saw %v over the bytes this encoder really "+
					"produces, want exactly [%s] — an unseen ref is an "+
					"unstamped atproto_blobs row, which is deleted user media",
					got, cid)
			}
		})
	}
}
