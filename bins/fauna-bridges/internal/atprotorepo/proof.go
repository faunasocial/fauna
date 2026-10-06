package atprotorepo

import (
	"bytes"
	"context"
	"errors"
	"fmt"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/repo"
	"github.com/bluesky-social/indigo/atproto/repo/mst"
	"github.com/bluesky-social/indigo/atproto/syntax"
	blocks "github.com/ipfs/go-block-format"
	"github.com/ipfs/go-cid"
)

// ErrRecordNotFound reports that a repo exists but holds no record at the
// requested (collection, rkey). Kept distinct from the no-such-repo error
// because the read surface owes the two callers different lexicon errors
// (RecordNotFound vs. RepoNotFound), and a proof request for a key a served
// repo simply does not have is an ordinary miss, not a failure.
var ErrRecordNotFound = errors.New("record not found in repo")

// maxProofDepth bounds the descent. An MST node's height comes from its keys'
// hashes, so a real tree is logarithmic in the record count and never remotely
// this deep (3 levels at 60 records) — the bound exists so a corrupt or cyclic
// tree cannot make one anonymous request walk forever, which on this surface is
// the same class of defect as an unbounded response. A hard-coded constant,
// never configuration, like the rest of the read surface's bounds.
const maxProofDepth = 64

// ExportRecordProof serializes the com.atproto.sync.getRecord payload: a CARv1
// rooted at the head commit carrying exactly the blocks that prove the record
// at (collection, rkey) is in the repo at that head — the signed commit, every
// MST node on the path from the repo root down to the record's key, and the
// record block itself.
//
// Why this is not ExportRepo. sync.getRecord served the FULL repo CAR until
// this existed, which is correct (the whole repo trivially contains the proof
// path) but is an amplification on an anonymous, unauthenticated surface: one
// request naming one record cost the network a response scaled to the user's
// entire projected history, not to anything the PDS chose. That is the same
// defect the listRepos/listRecords page ceilings closed, in the one place a
// ceiling cannot fix — the caller is already asking for a single record.
//
// The walk is deliberately LAZY: one block read per MST level (depth is
// logarithmic in the record count), never a whole-tree load. Loading the tree
// through mst.LoadTreeFromStore and selecting the path afterwards would shrink
// the response while leaving the work per request proportional to the repo, so
// the amplification would only move from the wire to the disk.
//
// Node decoding goes through indigo's own mst.NodeDataFromCBOR / NodeData.Node,
// so nothing here re-implements the MST wire format (§ Architecture: the bridge
// does not hand-roll MST/CBOR/signing). What is ours is only the descent — the
// choice of which child covers the key — and the CAR framing.
func (s *Store) ExportRecordProof(ctx context.Context, did, collection, rkey string) ([]byte, error) {
	h, ok, err := loadHead(ctx, s.db, did)
	if err != nil {
		return nil, err
	}
	if !ok {
		return nil, fmt.Errorf("no repo for did %s", did)
	}
	commitCID, err := cid.Decode(h.commitCID)
	if err != nil {
		return nil, fmt.Errorf("decode head commit cid: %w", err)
	}

	bs := newMemBlockstore()
	order := make([]cid.Cid, 0, 8)
	put := func(c cid.Cid, raw []byte) error {
		b, err := blocks.NewBlockWithCid(raw, c)
		if err != nil {
			return err
		}
		if err := bs.Put(ctx, b); err != nil {
			return err
		}
		order = append(order, c)
		return nil
	}

	commitRaw, found, err := s.GetBlock(ctx, did, []byte(commitCID.KeyString()))
	if err != nil {
		return nil, err
	}
	if !found {
		return nil, fmt.Errorf("head commit block %s missing from repo %s", commitCID, did)
	}
	if err := put(commitCID, commitRaw); err != nil { // commit root first, per the repo spec
		return nil, err
	}
	var commit repo.Commit
	if err := commit.UnmarshalCBOR(bytes.NewReader(commitRaw)); err != nil {
		return nil, fmt.Errorf("decode head commit: %w", err)
	}

	// Descend the MST from the commit's data root, collecting each node on the
	// way. A nil next child ends the walk: on a hit that node held the record's
	// CID, on a miss no child at that level can cover the key. The collected
	// path is what a non-existence proof would need, so serving one later is
	// additive — today the miss discards it and reports ErrRecordNotFound.
	key := []byte(collection + "/" + rkey)
	var recordCID *cid.Cid
	for next, depth := &commit.Data, 0; next != nil; depth++ {
		if depth > maxProofDepth {
			return nil, fmt.Errorf("mst walk for %s exceeded %d levels in repo %s (corrupt tree)", key, maxProofDepth, did)
		}
		nodeCID := *next
		raw, found, err := s.GetBlock(ctx, did, []byte(nodeCID.KeyString()))
		if err != nil {
			return nil, err
		}
		if !found {
			return nil, fmt.Errorf("mst node %s on the proof path is missing from repo %s", nodeCID, did)
		}
		if err := put(nodeCID, raw); err != nil {
			return nil, err
		}
		nd, err := mst.NodeDataFromCBOR(bytes.NewReader(raw))
		if err != nil {
			return nil, fmt.Errorf("decode mst node %s: %w", nodeCID, err)
		}
		node := nd.Node(&nodeCID)
		child, val := descendToKey(&node, key)
		if val != nil {
			recordCID = val
			break
		}
		next = child
	}
	if recordCID == nil {
		return nil, ErrRecordNotFound
	}

	recordRaw, found, err := s.GetBlock(ctx, did, []byte(recordCID.KeyString()))
	if err != nil {
		return nil, err
	}
	if !found {
		return nil, fmt.Errorf("record block %s missing from repo %s", recordCID, did)
	}
	if err := put(*recordCID, recordRaw); err != nil {
		return nil, err
	}
	return writeCAR(ctx, commitCID, bs, order)
}

// VerifyRecordProof is ExportRecordProof's inverse: it takes a
// com.atproto.sync.getRecord payload from a THIRD-PARTY PDS and returns the
// record's bytes only if the proof actually establishes them.
//
// # Why this exists
//
// The permission-set resolution chain (atproto-pds-full.md § F4 detail →
// *Permission sets*, "The chain is authenticated end-to-end") fetches a
// published Lexicon document whose contents then widen an OAuth grant. The
// hosting PDS is not the authority: most sets live on big multi-tenant hosts,
// and what feeds an authorization decision is verified, never trusted. The
// Lexicon spec itself specifies no record authentication; this is deliberately
// stricter.
//
// # What is actually proven, and by whom
//
// Nothing here re-implements MST descent, CAR parsing or signature checking —
// the same rule ExportRecordProof follows, and it matters more on this side,
// because a verifier that disagreed with the exporter about which child covers
// a key would be a proof-forgery seam. So:
//
//   - go-car/v2 parses the CAR and re-hashes every block against its CID, so a
//     block whose bytes were altered in flight cannot enter the store at all;
//   - indigo's repo.LoadRepoFromCAR builds the repo from the commit root down,
//     and GetRecordBytes resolves the key through the MST from commit.Data —
//     that resolution IS the inclusion proof, and it can only succeed if every
//     node on the path was present in the CAR;
//   - commit.VerifySignature binds the whole tree to the authority's key.
//
// What is ours is only the ORDER and the two identity checks below.
//
// # The order is load-bearing
//
// The signature is verified BEFORE the record is resolved. Both orders reject
// the same inputs, but this one never spends MST descent on a repo whose
// commit was not signed by the expected key — the resolution is work performed
// on an attacker's payload, and doing it after the signature means an
// unauthenticated caller cannot direct it.
//
// A nil signingKey is a refusal, never a skip: "no key to check against" is
// exactly when a verification must fail closed, and an arm that silently
// accepted would make every caller's guarantee vacuous.
func VerifyRecordProof(ctx context.Context, carBytes []byte, did, collection, rkey string, signingKey atcrypto.PublicKey) ([]byte, error) {
	if signingKey == nil {
		return nil, fmt.Errorf("refusing to verify a record proof for %s with no signing key", did)
	}
	if len(carBytes) == 0 {
		return nil, fmt.Errorf("empty record proof for %s", did)
	}

	commit, rp, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(carBytes))
	if err != nil {
		return nil, fmt.Errorf("record proof for %s is not a loadable repo CAR: %w", did, err)
	}
	if err := commit.VerifyStructure(); err != nil {
		return nil, fmt.Errorf("record proof for %s has a malformed commit: %w", did, err)
	}
	// Substitution check, the commit-level twin of the DID document's. A
	// correctly-signed proof from somebody else's repo answers a question we
	// did not ask; without this the only thing tying the payload to the DID
	// would be the URL we happened to fetch it from.
	if commit.DID != did {
		return nil, fmt.Errorf("record proof declares did %q, asked about %q — refusing a substituted repo", commit.DID, did)
	}
	if err := commit.VerifySignature(signingKey); err != nil {
		return nil, fmt.Errorf("record proof for %s is not signed by the key its DID document names: %w", did, err)
	}

	recordBytes, _, err := rp.GetRecordBytes(ctx, syntax.NSID(collection), syntax.RecordKey(rkey))
	if err != nil {
		return nil, fmt.Errorf("record proof for %s does not establish %s/%s: %w", did, collection, rkey, err)
	}
	if len(recordBytes) == 0 {
		return nil, fmt.Errorf("record proof for %s resolved %s/%s to an empty block", did, collection, rkey)
	}
	// Verbatim. The caller hands these bytes to the pure expander, and a
	// re-encoding in between would reopen the gap the verification just closed.
	return recordBytes, nil
}

// descendToKey resolves one MST level for key. It returns (nil, value) when the
// node holds key itself, (child, nil) when the search continues into that
// child subtree, and (nil, nil) when the key cannot exist below this node.
//
// MST node entries are lexically sorted and interleave value entries with
// child pointers, where a child covers exactly the keys strictly between its
// neighbouring value entries (indigo's NodeData.Node folds the node's `Left`
// pointer into the entry list as the leading child, and decompresses each
// entry's prefix-shortened key into a whole one, so both are ordinary entries
// by the time we see them). So the child that could hold key is the one
// immediately preceding the first value entry that sorts after key — tracked as
// `child` and cleared each time a value entry is passed, since a child before a
// value entry we have already walked past cannot cover anything above it.
//
// The two entry predicates are tested independently, never as an ordered
// either/or: today's decode path splits a wire entry's value and subtree
// pointer into separate unfolded entries (pinned by the exclusivity test),
// but nothing here depends on that. A both-set entry acts as a value entry
// for its own key AND as the subtree pointer for the keys after it — the
// wire format's `t` is the entry's right subtree — instead of taking a
// child-only arm that skips the key comparison and reports a present record
// as ErrRecordNotFound. Value half first: for a smaller sought key the
// covering child is the one from BEFORE this entry, so this entry's own
// pointer must not be adopted until its key has been passed.
func descendToKey(node *mst.Node, key []byte) (child, value *cid.Cid) {
	for i := range node.Entries {
		e := &node.Entries[i]
		if e.IsValue() {
			switch bytes.Compare(key, e.Key) {
			case 0:
				return nil, e.Value
			case -1:
				return child, nil
			}
			child = nil
		}
		if e.IsChild() {
			child = e.ChildCID
		}
	}
	return child, nil
}
