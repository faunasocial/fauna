package main

import (
	"context"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
)

type countingSignerSource struct {
	calls  int
	signer atprotopds.RepoSigner
}

type nopSigner struct{}

func (nopSigner) HashAndSign([]byte) ([]byte, error) { return []byte{0}, nil }

func (c *countingSignerSource) RepoSigner(context.Context, []byte) (atprotopds.RepoSigner, error) {
	c.calls++
	return c.signer, nil
}

// The proxy path mints per forwarded request; the cache is what makes that
// affordable, and the TTL is what bounds the exposure it adds.
func TestCachedRepoSignersUnsealsOncePerTTLWindow(t *testing.T) {
	inner := &countingSignerSource{signer: nopSigner{}}
	c := newCachedRepoSigners(inner)
	actor := make([]byte, 32)

	for i := 0; i < 5; i++ {
		if _, err := c.RepoSigner(context.Background(), actor); err != nil {
			t.Fatal(err)
		}
	}
	if inner.calls != 1 {
		t.Fatalf("inner unseals = %d, want 1 inside the TTL", inner.calls)
	}

	// A different account is its own entry.
	other := make([]byte, 32)
	other[0] = 1
	if _, err := c.RepoSigner(context.Background(), other); err != nil {
		t.Fatal(err)
	}
	if inner.calls != 2 {
		t.Fatalf("inner unseals = %d, want 2 after a second account", inner.calls)
	}

	// Expiry forces a fresh unseal — the key must not outlive the TTL.
	short := &countingSignerSource{signer: nopSigner{}}
	cs := newCachedRepoSigners(short)
	cs.ttl = time.Millisecond
	if _, err := cs.RepoSigner(context.Background(), actor); err != nil {
		t.Fatal(err)
	}
	time.Sleep(5 * time.Millisecond)
	if _, err := cs.RepoSigner(context.Background(), actor); err != nil {
		t.Fatal(err)
	}
	if short.calls != 2 {
		t.Fatalf("inner unseals = %d, want 2 across an expired window", short.calls)
	}
}
