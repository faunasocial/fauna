package atprotorepo

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/repo"
	"github.com/bluesky-social/indigo/atproto/repo/mst"
	"github.com/bluesky-social/indigo/atproto/syntax"
	blocks "github.com/ipfs/go-block-format"
	"github.com/ipfs/go-cid"
	car "github.com/ipld/go-car/v2"
)

// carCIDs reads every block CID out of a CARv1 with go-car/v2 (an independent
// parser, which also re-verifies each CID against its bytes), plus the roots.
func carCIDs(t *testing.T, carBytes []byte) (roots []cid.Cid, blocks map[string]bool) {
	t.Helper()
	br, err := car.NewBlockReader(bytes.NewReader(carBytes))
	if err != nil {
		t.Fatalf("go-car/v2 rejected the CAR: %v", err)
	}
	blocks = map[string]bool{}
	for {
		blk, err := br.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatalf("go-car/v2 could not verify a block: %v", err)
		}
		blocks[blk.Cid().String()] = true
	}
	return br.Roots, blocks
}

// seedProofRepo commits n single-record posts into did's repo and returns the
// rkeys in commit order.
func seedProofRepo(t *testing.T, st *Store, f *Funnel, did string, key atcrypto.PrivateKey, n int) []string {
	t.Helper()
	ctx := context.Background()
	announced(t, st, did)
	rk := tids(n)
	for i, k := range rk {
		if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
			Action:      ActionCreate,
			Collection:  feedPost,
			Rkey:        k,
			RecordCBOR:  postRecord(t, fmt.Sprintf("post %d", i), "2026-01-01T00:00:00Z"),
			FaunaPostID: fmt.Sprintf("%04x", i),
		}}); err != nil {
			t.Fatalf("commit %d: %v", i, err)
		}
	}
	return rk
}

// TestExportRecordProofVerifiesThroughIndigo: the com.atproto.sync.getRecord
// payload must be a PROOF CAR — the signed commit, the MST nodes on the path
// from the repo root down to the record's key, and the record block — and
// nothing else. The assertion that matters is not about our own bytes: indigo's
// own repo loader (what a relay runs) must accept the partial CAR and resolve
// the record out of it, which is only possible if every node on the proof path
// is present and the commit still names the same MST root.
func TestExportRecordProofVerifiesThroughIndigo(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	pub, _ := key.PublicKey()
	did := "did:plc:proof000000000000000000000"

	const n = 40
	rk := seedProofRepo(t, st, f, did, key, n)
	target := rk[n/2]

	want, wantBytes, ok, err := st.GetRecord(ctx, did, feedPost, target)
	if err != nil || !ok {
		t.Fatalf("seeded record missing: ok=%v err=%v", ok, err)
	}
	_, headCommit, _, err := st.Head(ctx, did)
	if err != nil {
		t.Fatal(err)
	}

	proof, err := st.ExportRecordProof(ctx, did, feedPost, target)
	if err != nil {
		t.Fatalf("ExportRecordProof: %v", err)
	}

	// (1) indigo's own loader accepts the partial CAR, the commit still verifies
	// against the repo's signing key, and the record resolves through the MST.
	commit, rp, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(proof))
	if err != nil {
		t.Fatalf("indigo rejected the proof CAR: %v", err)
	}
	if err := commit.VerifyStructure(); err != nil {
		t.Errorf("proof CAR commit structure: %v", err)
	}
	if err := commit.VerifySignature(pub); err != nil {
		t.Errorf("proof CAR commit signature: %v", err)
	}
	gotCID, err := rp.GetRecordCID(ctx, syntax.NSID(feedPost), syntax.RecordKey(target))
	if err != nil {
		t.Fatalf("indigo could not resolve the record through the proof path: %v", err)
	}
	if gotCID.String() != want {
		t.Errorf("proof resolved record CID %s, want %s", gotCID, want)
	}
	gotBytes, _, err := rp.GetRecordBytes(ctx, syntax.NSID(feedPost), syntax.RecordKey(target))
	if err != nil {
		t.Fatalf("indigo could not read the record bytes out of the proof: %v", err)
	}
	if !bytes.Equal(gotBytes, wantBytes) {
		t.Errorf("proof record bytes differ from the stored record (%d vs %d bytes)", len(gotBytes), len(wantBytes))
	}

	// (2) Minimality — the whole point of the slice. The proof must carry the
	// target's record block and NO other record block: on an anonymous,
	// unauthenticated surface a one-record request that serves every record is
	// an amplification whose factor is the user's entire projected history.
	roots, blocks := carCIDs(t, proof)
	if len(roots) != 1 || roots[0].String() != headCommit {
		t.Errorf("proof CAR roots = %v, want [%s] (the head commit)", roots, headCommit)
	}
	if !blocks[want] {
		t.Errorf("proof CAR is missing the target record block %s", want)
	}
	recs, err := st.ListRecords(ctx, did, feedPost)
	if err != nil {
		t.Fatal(err)
	}
	if len(recs) != n {
		t.Fatalf("seeded %d records, store has %d", n, len(recs))
	}
	for _, r := range recs {
		if r.Rkey == target {
			continue
		}
		if blocks[r.RecordCID] {
			t.Errorf("proof CAR leaks a foreign record block: rkey=%s cid=%s", r.Rkey, r.RecordCID)
		}
	}

	// And it must be dramatically smaller than the full repo CAR it replaces.
	full, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	_, fullBlocks := carCIDs(t, full)
	if len(blocks) >= len(fullBlocks) {
		t.Errorf("proof CAR has %d blocks, full repo CAR has %d — not a proof", len(blocks), len(fullBlocks))
	}
	if len(proof) >= len(full) {
		t.Errorf("proof CAR is %d bytes, full repo CAR is %d — not minimal", len(proof), len(full))
	}
	t.Logf("proof: %d blocks / %d bytes; full repo: %d blocks / %d bytes",
		len(blocks), len(proof), len(fullBlocks), len(full))
}

// TestDescendToKeyBoundsTheExclusionWalk pins the descent rule directly,
// because the end-to-end proofs cannot: a stale child is only ever offered
// where a value entry is followed immediately by another value entry (no child
// between them), and descending it can never produce a WRONG record — MST
// subtrees partition the keyspace, so a key no child at this level covers is
// simply absent, and a spurious descent still ends in ErrRecordNotFound. What
// it costs is depth-many extra block reads per miss, on the anonymous surface
// whose bounded work is this slice's whole point. That cost is invisible from
// the CAR (a miss returns an error and discards its path), so it is asserted
// here, on the function, instead.
func TestDescendToKeyBoundsTheExclusionWalk(t *testing.T) {
	left, mid := fakeCID("left"), fakeCID("mid")
	v1, v2 := fakeCID("v1"), fakeCID("v2")

	// A node whose two value entries are adjacent: nothing covers the gap
	// between "bbb" and "ddd", so a key there cannot exist anywhere below.
	adjacent := &mst.Node{Entries: []mst.NodeEntry{
		{ChildCID: &left},
		{Key: []byte("bbb"), Value: &v1},
		{Key: []byte("ddd"), Value: &v2},
	}}
	if child, val := descendToKey(adjacent, []byte("ccc")); child != nil || val != nil {
		t.Errorf("key in an uncovered gap = (child=%v, val=%v), want (nil, nil) — a stale child sends the walk down a subtree that cannot hold it", child, val)
	}
	if child, val := descendToKey(adjacent, []byte("eee")); child != nil || val != nil {
		t.Errorf("key past the last entry with no trailing child = (child=%v, val=%v), want (nil, nil)", child, val)
	}

	// The ordinary shapes, for contrast: below everything, an exact hit, and a
	// gap that IS covered.
	covered := &mst.Node{Entries: []mst.NodeEntry{
		{ChildCID: &left},
		{Key: []byte("bbb"), Value: &v1},
		{ChildCID: &mid},
		{Key: []byte("ddd"), Value: &v2},
	}}
	if child, _ := descendToKey(covered, []byte("aaa")); child == nil || !child.Equals(left) {
		t.Errorf("key below every entry descended %v, want the leading child %v", child, left)
	}
	if child, val := descendToKey(covered, []byte("ccc")); val != nil || child == nil || !child.Equals(mid) {
		t.Errorf("key in a covered gap = (child=%v, val=%v), want the child between the entries (%v)", child, val, mid)
	}
	if child, val := descendToKey(covered, []byte("bbb")); child != nil || val == nil || !val.Equals(v1) {
		t.Errorf("exact hit = (child=%v, val=%v), want (nil, %v)", child, val, v1)
	}
	if child, val := descendToKey(covered, []byte("ddd")); child != nil || val == nil || !val.Equals(v2) {
		t.Errorf("exact hit on the last entry = (child=%v, val=%v), want (nil, %v)", child, val, v2)
	}
}

// fakeCID is a real, distinct CID derived from seed — descendToKey compares and
// returns CIDs but never dereferences them, so any well-formed CID will do.
func fakeCID(seed string) cid.Cid { return blocks.NewBlock([]byte(seed)).Cid() }

// TestExportRecordProofResolvesEveryKey: the descent rule — which child subtree
// covers a key — is the one piece of this path that is ours rather than
// indigo's, and a single sampled key does not exercise it (a mutation that
// wrongly offers a child sitting before an already-passed value entry survives
// a one-key test, because whether that case is reached depends on where the
// sampled key happens to sit in the tree). So sweep EVERY record in a repo
// large enough to be several MST levels deep, and prove each one through
// indigo's own loader.
func TestExportRecordProofResolvesEveryKey(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:plc:proof200000000000000000000"

	const n = 60
	rk := seedProofRepo(t, st, f, did, key, n)

	maxDepth := 0
	for i, k := range rk {
		wantCID, wantBytes, ok, err := st.GetRecord(ctx, did, feedPost, k)
		if err != nil || !ok {
			t.Fatalf("record %d (%s) missing from the store: ok=%v err=%v", i, k, ok, err)
		}
		proof, err := st.ExportRecordProof(ctx, did, feedPost, k)
		if err != nil {
			t.Fatalf("record %d (%s): ExportRecordProof: %v", i, k, err)
		}
		_, rp, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(proof))
		if err != nil {
			t.Fatalf("record %d (%s): indigo rejected the proof CAR: %v", i, k, err)
		}
		gotBytes, gotCID, err := rp.GetRecordBytes(ctx, syntax.NSID(feedPost), syntax.RecordKey(k))
		if err != nil {
			t.Fatalf("record %d (%s): indigo could not resolve it through the proof path: %v", i, k, err)
		}
		if gotCID.String() != wantCID {
			t.Errorf("record %d (%s): proof resolved CID %s, want %s", i, k, gotCID, wantCID)
		}
		if !bytes.Equal(gotBytes, wantBytes) {
			t.Errorf("record %d (%s): proof bytes differ from the stored record", i, k)
		}
		// Every proof carries the commit, the record, and the MST path — so the
		// block count is the tree depth plus two, and none of them may be another
		// record.
		_, blocks := carCIDs(t, proof)
		if d := len(blocks) - 2; d > maxDepth {
			maxDepth = d
		}
		if !blocks[wantCID] {
			t.Errorf("record %d (%s): proof is missing its own record block", i, k)
		}
	}
	if maxDepth < 2 {
		t.Fatalf("deepest proof path was %d MST nodes — the repo is too shallow to exercise the descent", maxDepth)
	}
	t.Logf("swept %d records; deepest proof path = %d MST nodes", n, maxDepth)
}

// TestExportRecordProofAbsentKeysAcrossTheTree: a miss must read as
// ErrRecordNotFound wherever the absent key would sort — before the first
// record, between two of them, and after the last — never as an error and never
// as a proof of something else.
func TestExportRecordProofAbsentKeysAcrossTheTree(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:plc:proof300000000000000000000"
	rk := seedProofRepo(t, st, f, did, key, 30)

	// Valid TID-shaped keys that sort before / among / after the seeded set, plus
	// a same-prefix near-miss on a real rkey (the case a prefix-compressed MST
	// entry comparison would get wrong).
	absent := []string{
		"2222222222222",
		"3aaaaaaaaaaaa",
		rk[10][:len(rk[10])-1] + "0",
		rk[20][:len(rk[20])-1] + "z",
		"7zzzzzzzzzzzz",
	}
	for _, k := range absent {
		if _, err := st.ExportRecordProof(ctx, did, feedPost, k); !errors.Is(err, ErrRecordNotFound) {
			t.Errorf("absent rkey %q = %v, want ErrRecordNotFound", k, err)
		}
	}
	// A different collection at a real rkey is equally absent.
	if _, err := st.ExportRecordProof(ctx, did, "app.bsky.feed.like", rk[0]); !errors.Is(err, ErrRecordNotFound) {
		t.Errorf("real rkey in an unprojected collection = %v, want ErrRecordNotFound", err)
	}
}

// TestExportRecordProofUnknownRecord: a proof for a key the repo does not hold
// is a distinguishable miss, not an error and not a silent empty CAR — the
// handler turns it into RecordNotFound.
func TestExportRecordProofUnknownRecord(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:plc:proof100000000000000000000"
	seedProofRepo(t, st, f, did, key, 3)

	if _, err := st.ExportRecordProof(ctx, did, feedPost, "3knothinghere"); !errors.Is(err, ErrRecordNotFound) {
		t.Errorf("proof for an absent rkey = %v, want ErrRecordNotFound", err)
	}
	if _, err := st.ExportRecordProof(ctx, "did:plc:nosuchrepo00000000000000", feedPost, "3knothinghere"); errors.Is(err, ErrRecordNotFound) {
		t.Error("an unknown REPO must not read as a missing record — the handler owes RepoNotFound")
	}
}

// proofFixture seeds a repo, returns a proof CAR for one record plus everything
// needed to verify it: the DID, the signing key's public half, the rkey, and
// the record's stored bytes.
func proofFixture(t *testing.T) (proof []byte, did, rkey string, pub atcrypto.PublicKey, recordBytes []byte) {
	t.Helper()
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	pub, _ = key.PublicKey()
	did = "did:plc:verify00000000000000000000"

	const n = 40
	rk := seedProofRepo(t, st, f, did, key, n)
	rkey = rk[n/2]

	_, recordBytes, ok, err := st.GetRecord(ctx, did, feedPost, rkey)
	if err != nil || !ok {
		t.Fatalf("seeded record missing: ok=%v err=%v", ok, err)
	}
	proof, err = st.ExportRecordProof(ctx, did, feedPost, rkey)
	if err != nil {
		t.Fatalf("ExportRecordProof: %v", err)
	}
	return proof, did, rkey, pub, recordBytes
}

// TestVerifyRecordProofAcceptsAnHonestProof — the round trip that makes the
// permission-set resolution chain possible: what ExportRecordProof emits is
// exactly what VerifyRecordProof accepts, and the bytes come back verbatim
// (the expander is handed the VERIFIED block, never a re-encoding of it).
func TestVerifyRecordProofAcceptsAnHonestProof(t *testing.T) {
	proof, did, rkey, pub, want := proofFixture(t)

	got, err := VerifyRecordProof(context.Background(), proof, did, feedPost, rkey, pub)
	if err != nil {
		t.Fatalf("VerifyRecordProof rejected an honest proof: %v", err)
	}
	if !bytes.Equal(got, want) {
		t.Errorf("record bytes differ from the stored record (%d vs %d bytes)", len(got), len(want))
	}
}

// TestVerifyRecordProofRefusalArms — the whole reason this function exists.
// The hosting PDS is NOT the authority (atproto-pds-full.md § F4 detail: what
// feeds an authorization decision is verified, never trusted), so every way a
// hostile or broken host can answer must be a failed resolution.
func TestVerifyRecordProofRefusalArms(t *testing.T) {
	ctx := context.Background()
	proof, did, rkey, pub, _ := proofFixture(t)

	t.Run("a signature from the wrong key", func(t *testing.T) {
		// The heart of it: a PDS that serves a repo it signed itself, for a DID
		// whose document names someone else's key.
		other, err := atcrypto.GeneratePrivateKeyK256()
		if err != nil {
			t.Fatal(err)
		}
		otherPub, _ := other.PublicKey()
		if _, err := VerifyRecordProof(ctx, proof, did, feedPost, rkey, otherPub); err == nil {
			t.Fatal("want a refusal when the commit is signed by a key the DID document does not name")
		}
	})

	t.Run("a proof for a different DID", func(t *testing.T) {
		// A correctly-signed proof from someone else's repo is still not an
		// answer about the DID we asked about.
		if _, err := VerifyRecordProof(ctx, proof, "did:plc:someoneelse0000000000000", feedPost, rkey, pub); err == nil {
			t.Fatal("want a refusal when the commit names a different DID")
		}
	})

	t.Run("a nil signing key", func(t *testing.T) {
		// Never vacuously pass: no key means no verification is possible, which
		// is a refusal, not a skip.
		if _, err := VerifyRecordProof(ctx, proof, did, feedPost, rkey, nil); err == nil {
			t.Fatal("want a refusal when no signing key is supplied")
		}
	})

	t.Run("a record the proof does not cover", func(t *testing.T) {
		// An honest, correctly-signed proof for record A is not evidence about
		// record B — the inclusion proof is per-key.
		if _, err := VerifyRecordProof(ctx, proof, did, feedPost, "3zzzzzzzzzzzz", pub); err == nil {
			t.Fatal("want a refusal for an rkey the proof path does not cover")
		}
	})

	t.Run("a wrong collection", func(t *testing.T) {
		if _, err := VerifyRecordProof(ctx, proof, did, "app.bsky.feed.like", rkey, pub); err == nil {
			t.Fatal("want a refusal for a collection the proof path does not cover")
		}
	})

	t.Run("a tampered record block", func(t *testing.T) {
		// Flip a byte inside the record payload, keeping the CAR framing intact.
		// The block no longer hashes to the CID the signed MST names, so the
		// chain from the signature to the bytes is broken.
		tampered := bytes.Clone(proof)
		marker := []byte("post 20")
		i := bytes.Index(tampered, marker)
		if i < 0 {
			t.Skip("record payload marker not found in this fixture")
		}
		tampered[i+5] = 'X'
		if _, err := VerifyRecordProof(ctx, tampered, did, feedPost, rkey, pub); err == nil {
			t.Fatal("want a refusal when a block does not hash to its CID")
		}
	})

	t.Run("not a CAR at all", func(t *testing.T) {
		if _, err := VerifyRecordProof(ctx, []byte("this is not a CAR"), did, feedPost, rkey, pub); err == nil {
			t.Fatal("want a refusal for an unparseable payload")
		}
	})

	t.Run("an empty body", func(t *testing.T) {
		if _, err := VerifyRecordProof(ctx, nil, did, feedPost, rkey, pub); err == nil {
			t.Fatal("want a refusal for an empty payload")
		}
	})
}
