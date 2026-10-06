package atprotorepo

import (
	"context"
	"sync"

	blocks "github.com/ipfs/go-block-format"
	"github.com/ipfs/go-cid"
	ipld "github.com/ipfs/go-ipld-format"
)

// memBlockstore is an in-memory blockstore that preserves block CIDs EXACTLY
// as inserted (keyed by the full cid.KeyString, codec included).
//
// Graduated verbatim-in-behaviour from the S0 probe's blockstore.go (the
// throwaway tools/atproto-s0-probe, removed 2026-10-05), which self-validated through
// indigo's own repo loader. We cannot use github.com/ipfs/go-ipfs-blockstore
// here: its AllKeysChan normalizes every key to a raw-codec (0x55) CID, so the
// dag-cbor (0x71) MST node and commit blocks would be written under the wrong
// CID and neither the relay nor our own reload via repo.LoadRepoFromCAR could
// resolve the MST root. Preserving the codec is the whole point — this is the
// exact-CID keystring discipline the per-user commit funnel depends on.
//
// It implements the github.com/ipfs/go-ipfs-blockstore.Blockstore method set
// structurally, so it satisfies mst.Tree.WriteDiffBlocks's parameter.
type memBlockstore struct {
	mu     sync.Mutex
	blocks map[string]blocks.Block
}

func newMemBlockstore() *memBlockstore {
	return &memBlockstore{blocks: make(map[string]blocks.Block)}
}

func (m *memBlockstore) Put(_ context.Context, b blocks.Block) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.blocks[b.Cid().KeyString()] = b
	return nil
}

func (m *memBlockstore) PutMany(ctx context.Context, bs []blocks.Block) error {
	for _, b := range bs {
		if err := m.Put(ctx, b); err != nil {
			return err
		}
	}
	return nil
}

func (m *memBlockstore) Get(_ context.Context, c cid.Cid) (blocks.Block, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	b, ok := m.blocks[c.KeyString()]
	if !ok {
		return nil, ipld.ErrNotFound{Cid: c}
	}
	return b, nil
}

func (m *memBlockstore) Has(_ context.Context, c cid.Cid) (bool, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	_, ok := m.blocks[c.KeyString()]
	return ok, nil
}

func (m *memBlockstore) GetSize(_ context.Context, c cid.Cid) (int, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	b, ok := m.blocks[c.KeyString()]
	if !ok {
		return -1, ipld.ErrNotFound{Cid: c}
	}
	return len(b.RawData()), nil
}

func (m *memBlockstore) DeleteBlock(_ context.Context, c cid.Cid) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	delete(m.blocks, c.KeyString())
	return nil
}

func (m *memBlockstore) AllKeysChan(ctx context.Context) (<-chan cid.Cid, error) {
	m.mu.Lock()
	cids := make([]cid.Cid, 0, len(m.blocks))
	for _, b := range m.blocks {
		cids = append(cids, b.Cid())
	}
	m.mu.Unlock()

	ch := make(chan cid.Cid, len(cids))
	for _, c := range cids {
		ch <- c
	}
	close(ch)
	return ch, nil
}

func (m *memBlockstore) HashOnRead(bool) {}

// allBlocks returns every block currently held (unspecified order).
func (m *memBlockstore) allBlocks() []blocks.Block {
	m.mu.Lock()
	defer m.mu.Unlock()
	out := make([]blocks.Block, 0, len(m.blocks))
	for _, b := range m.blocks {
		out = append(out, b)
	}
	return out
}
