package atprotorepo

import (
	"bytes"
	"context"
	"fmt"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/atdata"
	"github.com/bluesky-social/indigo/atproto/repo"
	"github.com/bluesky-social/indigo/atproto/syntax"
)

const actorProfile = "app.bsky.actor.profile"

// profileRecord builds a minimal app.bsky.actor.profile record whose bytes
// change with displayName — the update-heavy shape: rkey "self", rewritten in
// place, so every rewrite grows the persisted block table while the live set
// keeps exactly one record for it.
func profileRecord(t *testing.T, displayName string) []byte {
	t.Helper()
	b, err := atdata.MarshalCBOR(map[string]any{
		"$type":       actorProfile,
		"displayName": displayName,
	})
	if err != nil {
		t.Fatalf("marshal profile record: %v", err)
	}
	return b
}

// liveSet derives the repo's current state independently of ExportRepo: the
// union over every live record of its proof CAR. ExportRecordProof is pinned by
// its own suite, every MST node lies on the root-to-key path of at least one
// key beneath it, and every proof carries the head commit — so the union is
// exactly the head commit + every live MST node + every live record block.
func liveSet(t *testing.T, st *Store, did string, live map[string][]string) map[string]bool {
	t.Helper()
	ctx := context.Background()
	want := map[string]bool{}
	for collection, rkeys := range live {
		for _, rk := range rkeys {
			proof, err := st.ExportRecordProof(ctx, did, collection, rk)
			if err != nil {
				t.Fatalf("proof for %s/%s: %v", collection, rk, err)
			}
			_, pb := carCIDs(t, proof)
			for c := range pb {
				want[c] = true
			}
		}
	}
	return want
}

// TestExportRepoServesTheCurrentStateOnly: the com.atproto.sync.getRepo payload
// is specified as the repo's CURRENT state — "all repo records, MST nodes, and
// the current signed commit object, all in a single CAR file" (atproto sync
// spec; the repository spec's full export is likewise "the full repo structure
// … for the indicated commit") — never the superseded commits, MST nodes and
// record blocks the append-only store also holds. Network-side chain validation
// is inductive (`prevData` on #commit frames); nothing dereferences the head
// commit's prev out of a getRepo response, so on-box history is not payload.
//
// The wanted set is derived independently as the union of every live record's
// proof CAR, so the set equality fails in BOTH directions: a block served from
// outside the current state (the amplification this closes — superseded blocks
// made the export scale with commit count, ~4x blocks / ~8x bytes for
// append-only repos and unbounded for update-heavy ones), and a live block
// missing (a broken walk).
func TestExportRepoServesTheCurrentStateOnly(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	pub, _ := key.PublicKey()
	did := "did:plc:liveset0000000000000000000"

	// A history-heavy repo: 12 posts, a profile singleton rewritten 8 times
	// (same rkey, new bytes each pass), 2 posts deleted. The live set lands at
	// 11 records; the persisted table holds every superseded commit, record and
	// MST path along the way.
	rk := seedProofRepo(t, st, f, did, key, 12)
	for i := 0; i < 8; i++ {
		if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
			Action: ActionUpdate, Collection: actorProfile, Rkey: "self",
			RecordCBOR: profileRecord(t, fmt.Sprintf("Alice v%d", i)),
		}}); err != nil {
			t.Fatalf("profile rewrite %d: %v", i, err)
		}
	}
	deleted := map[int]bool{3: true, 7: true}
	for i := range deleted {
		if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
			Action: ActionDelete, Collection: feedPost, Rkey: rk[i],
			FaunaPostID: fmt.Sprintf("%04x", i),
		}}); err != nil {
			t.Fatalf("delete %s: %v", rk[i], err)
		}
	}
	live := map[string][]string{actorProfile: {"self"}}
	for i, k := range rk {
		if !deleted[i] {
			live[feedPost] = append(live[feedPost], k)
		}
	}

	export, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatalf("ExportRepo: %v", err)
	}
	roots, got := carCIDs(t, export)

	// (1) Rooted at the head commit; indigo's own loader (what a relay runs)
	// accepts it, the signature verifies, and every live record resolves.
	_, headCommit, _, err := st.Head(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	if len(roots) != 1 || roots[0].String() != headCommit {
		t.Errorf("export roots = %v, want [%s] (the head commit)", roots, headCommit)
	}
	commit, rr, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(export))
	if err != nil {
		t.Fatalf("indigo rejected the export: %v", err)
	}
	if err := commit.VerifyStructure(); err != nil {
		t.Errorf("export commit structure: %v", err)
	}
	if err := commit.VerifySignature(pub); err != nil {
		t.Errorf("export commit signature: %v", err)
	}
	for collection, rkeys := range live {
		for _, k := range rkeys {
			if _, _, err := rr.GetRecordBytes(ctx, syntax.NSID(collection), syntax.RecordKey(k)); err != nil {
				t.Errorf("live record %s/%s not resolvable from the export: %v", collection, k, err)
			}
		}
	}

	// (2) Set equality with the independently-derived live set.
	want := liveSet(t, st, did, live)
	for c := range got {
		if !want[c] {
			t.Errorf("export serves a block outside the current state (superseded commit/MST/record): %s", c)
		}
	}
	for c := range want {
		if !got[c] {
			t.Errorf("export is missing a live block a record proof needs: %s", c)
		}
	}
	t.Logf("export: %d blocks / %d bytes; live set: %d blocks", len(got), len(export), len(want))

	// (3) Rewrite invariance — the update-heavy argument as one number: the key
	// set is unchanged, so the MST shape is unchanged, so K further rewrites of
	// one record must leave the export's block COUNT exactly where it was.
	countBefore := len(got)
	for i := 0; i < 5; i++ {
		if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
			Action: ActionUpdate, Collection: actorProfile, Rkey: "self",
			RecordCBOR: profileRecord(t, fmt.Sprintf("Alice rewrite %d", i)),
		}}); err != nil {
			t.Fatalf("profile rewrite (invariance) %d: %v", i, err)
		}
	}
	export2, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	_, got2 := carCIDs(t, export2)
	if len(got2) != countBefore {
		t.Errorf("export block count after 5 rewrites = %d, want %d — the payload must track the live repo, not the commit count", len(got2), countBefore)
	}

	// (4) The all-deleted repo is still a repo: exactly the head commit plus
	// the empty MST root, and indigo still loads it.
	for collection, rkeys := range live {
		for _, k := range rkeys {
			if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
				Action: ActionDelete, Collection: collection, Rkey: k,
			}}); err != nil {
				t.Fatalf("final delete %s/%s: %v", collection, k, err)
			}
		}
	}
	empty, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	_, emptyBlocks := carCIDs(t, empty)
	if len(emptyBlocks) != 2 {
		t.Errorf("all-deleted export = %d blocks, want 2 (head commit + empty MST root)", len(emptyBlocks))
	}
	if _, _, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(empty)); err != nil {
		t.Errorf("indigo rejected the all-deleted export: %v", err)
	}
}
