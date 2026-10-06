package atprotorepo

// The pins: both repo walks —
// ExportRepo's node walk and descendToKey — read indigo's unfolded MST entry
// list through the IsChild()/IsValue() predicate pair. Nothing in the
// dependency's API PROMISES those predicates are mutually exclusive on one
// entry, and the walks must not assume it: an ordered either/or that walks
// the child and skips the value would turn a both-set entry into a short CAR
// that parses as a complete repo (the export) or a silent ErrRecordNotFound
// for a record that exists (the proof path). The tests here pin, by
// EXECUTION and never by reading the dependency's source (the iron-clad
// rule), the three facts the restructured walks rest on:
//
//  1. IsChild() ≡ (ChildCID != nil) and IsValue() ≡ (Value != nil) — so
//     there is no third legitimate entry form, and the corrupt arm fires
//     only on an entry naming nothing;
//  2. today's decode path splits a wire entry's value and subtree pointer
//     into separate unfolded entries (a dependency bump that changes this
//     lands as a red test, not a production mystery);
//  3. descendToKey answers a both-set entry's own key with its value.

import (
	"bytes"
	"context"
	"fmt"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/repo"
	"github.com/bluesky-social/indigo/atproto/repo/mst"
	"github.com/ipfs/go-cid"
)

// entryCID mints a real, distinct CIDv1 from a label — an invented base32
// literal fails multihash parsing, so fixture CIDs are computed, never typed.
func entryCID(t *testing.T, label string) cid.Cid {
	t.Helper()
	c, err := BlobCIDForBytes([]byte(label))
	if err != nil {
		t.Fatalf("mint CID for %q: %v", label, err)
	}
	return c
}

// TestMSTEntryPredicatesAreExactlyTheCIDNilChecks pins the executed
// equivalence IsChild() ≡ (ChildCID != nil), IsValue() ≡ (Value != nil) over
// every constructible entry shape. Two consequences hang off this table: an
// entry naming neither a child nor a value is definitionally corrupt (the
// walks' fail arm cannot fire on a healthy tree), and both predicates CAN
// hold on one constructed entry — which is why the walks test them
// independently rather than as an ordered switch.
func TestMSTEntryPredicatesAreExactlyTheCIDNilChecks(t *testing.T) {
	c := entryCID(t, "predicate-table")
	key := []byte("app.bsky.feed.post/3jzfcijpj2z2a")
	cases := []struct {
		name             string
		entry            mst.NodeEntry
		isChild, isValue bool
	}{
		{"zero value", mst.NodeEntry{}, false, false},
		{"child only", mst.NodeEntry{ChildCID: &c}, true, false},
		{"key+value", mst.NodeEntry{Key: key, Value: &c}, false, true},
		{"both set", mst.NodeEntry{Key: key, Value: &c, ChildCID: &c}, true, true},
		{"key, nil value", mst.NodeEntry{Key: key}, false, false},
	}
	for _, tc := range cases {
		if got := tc.entry.IsChild(); got != tc.isChild {
			t.Errorf("%s: IsChild() = %v, want %v — the equivalence the walks' belt-and-braces guards assume has changed", tc.name, got, tc.isChild)
		}
		if got := tc.entry.IsValue(); got != tc.isValue {
			t.Errorf("%s: IsValue() = %v, want %v — the equivalence the walks' belt-and-braces guards assume has changed", tc.name, got, tc.isValue)
		}
	}
}

// TestRealRepoMSTNodesSplitChildAndValueEntriesExclusively walks every MST
// node of a real multi-level repo and asserts the representation invariant
// the walks no longer DEPEND on but descendToKey's key-coverage reasoning is
// still worded in: the unfolded entry list holds child-only and value-only
// entries, never a both-set or neither-set one. childOnly + valueOnly ==
// entries is the whole assertion; the childOnly floor proves the fixture
// actually forced interior child pointers (a single-node tree would pin
// nothing).
func TestRealRepoMSTNodesSplitChildAndValueEntriesExclusively(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:plc:entrysplit0000000000000000"
	seedProofRepo(t, st, f, did, key, 400)

	_, headCommit, _, err := st.Head(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	commitCID, err := cid.Parse(headCommit)
	if err != nil {
		t.Fatalf("parse head commit CID %q: %v", headCommit, err)
	}
	commitRaw, found, err := st.GetBlock(ctx, did, []byte(commitCID.KeyString()))
	if err != nil || !found {
		t.Fatalf("load head commit: found=%v err=%v", found, err)
	}
	var commit repo.Commit
	if err := commit.UnmarshalCBOR(bytes.NewReader(commitRaw)); err != nil {
		t.Fatalf("decode head commit: %v", err)
	}

	var nodes, entries, childOnly, valueOnly, both, neither int
	visited := map[string]bool{}
	var walk func(nodeCID cid.Cid) error
	walk = func(nodeCID cid.Cid) error {
		if visited[nodeCID.KeyString()] {
			return nil
		}
		visited[nodeCID.KeyString()] = true
		raw, ok, err := st.GetBlock(ctx, did, []byte(nodeCID.KeyString()))
		if err != nil || !ok {
			return fmt.Errorf("load mst node %s: found=%v err=%v", nodeCID, ok, err)
		}
		nd, err := mst.NodeDataFromCBOR(bytes.NewReader(raw))
		if err != nil {
			return fmt.Errorf("decode mst node %s: %v", nodeCID, err)
		}
		node := nd.Node(&nodeCID)
		nodes++
		for i := range node.Entries {
			e := &node.Entries[i]
			entries++
			switch {
			case e.IsChild() && e.IsValue():
				both++
			case e.IsChild():
				childOnly++
				if err := walk(*e.ChildCID); err != nil {
					return err
				}
			case e.IsValue():
				valueOnly++
			default:
				neither++
			}
		}
		return nil
	}
	if err := walk(commit.Data); err != nil {
		t.Fatal(err)
	}

	if both != 0 || neither != 0 {
		t.Errorf("over %d nodes / %d entries: both-set=%d neither-set=%d, want 0/0 — the dependency's unfolded representation changed; re-audit descendToKey's key-coverage ordering before trusting this bump", nodes, entries, both, neither)
	}
	if childOnly+valueOnly != entries-both-neither {
		t.Errorf("entry classes do not partition: childOnly=%d valueOnly=%d entries=%d", childOnly, valueOnly, entries)
	}
	if childOnly == 0 {
		t.Fatalf("no interior child pointers over %d records — the fixture did not force a multi-level tree, so this pins nothing", 400)
	}
}

// TestDescendToKeyFindsAValueOnABothSetEntry: descendToKey must not assume
// child and value are mutually exclusive on one entry. Before the
// restructure a both-set entry took the child arm and skipped the key
// comparison, so a record present AT that entry resolved to a silent
// ErrRecordNotFound — the invisible-failure class the export walk's corrupt
// arm exists to prevent. The entry is constructed directly (today's decode
// path always splits — see the exclusivity pin above), so this is the
// shape-robustness pin, not a reachable-today repro.
func TestDescendToKeyFindsAValueOnABothSetEntry(t *testing.T) {
	child := entryCID(t, "subtree")
	val := entryCID(t, "record")
	key := []byte("app.bsky.feed.post/3jzfcijpj2z2a")
	node := mst.Node{Entries: []mst.NodeEntry{
		{ChildCID: &child, Key: key, Value: &val},
	}}
	gotChild, gotVal := descendToKey(&node, key)
	if gotVal == nil || !gotVal.Equals(val) {
		t.Fatalf("descendToKey = (%v, %v), want the entry's own value — the key IS present at this entry", gotChild, gotVal)
	}
}

// TestDescendToKeyBothSetEntryStillCoversTheKeysAfterIt: the wire format's
// subtree pointer (`t`) covers the keys AFTER its entry's key, so a both-set
// entry must keep acting as the candidate child for a larger sought key —
// finding the value half must not eat the child half.
func TestDescendToKeyBothSetEntryStillCoversTheKeysAfterIt(t *testing.T) {
	child := entryCID(t, "right-subtree")
	val := entryCID(t, "record-2")
	node := mst.Node{Entries: []mst.NodeEntry{
		{ChildCID: &child, Key: []byte("app.bsky.feed.post/aaa"), Value: &val},
	}}
	gotChild, gotVal := descendToKey(&node, []byte("app.bsky.feed.post/zzz"))
	if gotVal != nil {
		t.Fatalf("sought key is not at this entry, but got value %v", gotVal)
	}
	if gotChild == nil || !gotChild.Equals(child) {
		t.Fatalf("descendToKey child = %v, want the entry's subtree pointer — a larger key descends through it", gotChild)
	}
}
