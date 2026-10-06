package main

// The production atprotopds.RepoSignerSource: resolve one account's repo
// signing key by fetching its sealed identity-key blob from nest and
// HPKE-opening it (atproto-pds-full.md § Key material inventory — the repo
// signing key signs repo commits *and* service JWTs, which is what keeps C7's
// "no custody widening" true).
//
// This lives in cmd/ rather than internal/atprotopds for the same reason
// ffiAuthorizer and ffiFetchGuard do: the unseal is cgo, and the handler
// package stays cgo-free behind its seam.

import (
	"context"
	"encoding/hex"
	"fmt"
	"sync"
	"time"

	"github.com/bluesky-social/indigo/atproto/atcrypto"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// unsealIdentityBlobFn opens a sealed AtprotoIdentityBlob against the
// identity's two published keys (production: mailfauna.UnsealAtprotoIdentityBlob
// over the FFI, where the shared Rust refuses a blob whose keys are not those;
// tests substitute).
type unsealIdentityBlobFn func(
	blobBytes, recipientX25519Secret []byte,
	expectedSigningPubDIDKey, expectedRotationPubDIDKey string,
) (*mailfauna.AtprotoIdentityKeyBundle, error)

// openIdentityKeys is the ONE fetch + unseal of an identity's sealed keys —
// the mint, the rotation-key path and the repo signer all come through here.
//
// It exists so the expectation cannot be forgotten: the fetch reply carries the
// identity's published keys beside the blob, and the unseal is handed exactly
// those. One identity's whole blob served where another's was asked for — a
// routing bug, a swapped row — then fails here instead of signing. The binding
// is to the published keys and never to the actor id, because the blob moves to
// a successor unchanged and names an ancestor for the life of the DID.
//
// Not a defence against a hostile nest: the published keys are nest-sourced,
// and the nest minted these keys in the first place.
//
// The caller owns the returned scalars and zeroizes them.
func openIdentityKeys(
	ctx context.Context,
	c wsrpc.Caller,
	unseal unsealIdentityBlobFn,
	x25519Secret, actorID []byte,
) (*mailfauna.AtprotoIdentityKeyBundle, error) {
	keyBlob, err := wsrpc.FetchAtprotoIdentityKeyBlob(ctx, c, actorID)
	if err != nil {
		return nil, err
	}
	bundle, err := unseal(keyBlob.Blob, x25519Secret, keyBlob.SigningPubDIDKey, keyBlob.BridgeRotationPubDIDKey)
	if err != nil {
		return nil, fmt.Errorf("unseal identity key blob: %w", err)
	}
	return bundle, nil
}

// repoSignerForActor fetches + unseals one account's identity key blob and
// returns its repo-commit signing key (K-256). The unsealed scalars are
// zeroized the moment the key is parsed — no on-disk copies, the
// short-lifetime discipline the mint loop follows.
//
// One implementation, two callers: the S3 projection loop signs repo commits
// with it, and F3's service-auth mint signs JWTs with it. A second copy would
// be a second place for the curve check and the zeroize to drift.
func repoSignerForActor(
	ctx context.Context,
	c wsrpc.Caller,
	unseal unsealIdentityBlobFn,
	x25519Secret, actorID []byte,
) (atcrypto.PrivateKeyExportable, error) {
	bundle, err := openIdentityKeys(ctx, c, unseal, x25519Secret, actorID)
	if err != nil {
		return nil, err
	}
	defer zeroize(bundle.SigningPriv)
	defer zeroize(bundle.RotationPriv)
	if bundle.SigningCurve != "k256" {
		return nil, fmt.Errorf("unsupported signing curve %q (want k256)", bundle.SigningCurve)
	}
	signingKey, err := atprotoid.PrivateKeyFromK256Scalar(bundle.SigningPriv)
	if err != nil {
		return nil, fmt.Errorf("repo signing key from scalar: %w", err)
	}
	// The seal-drift self-check the rotation key already gets, for the key that
	// signs every commit: the unseal bound the bundle's recorded pubkey to the
	// identity's published one, and this binds the SCALAR to that record — so
	// what signs is what the DID document lists.
	derived, err := atprotoid.DIDKeyForPrivate(signingKey)
	if err != nil {
		return nil, fmt.Errorf("derive signing did:key: %w", err)
	}
	if bundle.SigningPubDidKey != "" && derived != bundle.SigningPubDidKey {
		return nil, fmt.Errorf("unsealed signing scalar derives %s but nest recorded %s (seal drift)",
			derived, bundle.SigningPubDidKey)
	}
	return signingKey, nil
}

// sealedRepoSigners is the atprotopds.RepoSignerSource wired in production.
//
// Deliberately uncached: an explicit getServiceAuth call is rare, and a cache
// of unsealed private keys is a real exposure that should be bought only when
// something needs it. The proxy path — which needs a key on every forwarded
// request rather than once per explicit mint — is what will need one, and the
// RepoSignerSource seam is where it goes, with no caller change.
type sealedRepoSigners struct {
	nest         wsrpc.Caller
	unseal       unsealIdentityBlobFn
	x25519Secret []byte
}

func (s sealedRepoSigners) RepoSigner(ctx context.Context, actorID []byte) (atprotopds.RepoSigner, error) {
	return repoSignerForActor(ctx, s.nest, s.unseal, s.x25519Secret, actorID)
}

// repoSignerCacheTTL bounds how long an unsealed repo signing key stays in
// bridge memory. The proxy path mints on EVERY forwarded request; a per-request
// nest fetch + HPKE unseal is not acceptable at that volume, and this TTL is
// the whole exposure the cache adds — expiry is checked on read, entries never
// outlive it. A revoked/rotated key therefore signs for at most this long
// after nest state changes, well inside the 60 min the access-token lifetime
// already tolerates.
const repoSignerCacheTTL = 5 * time.Minute

// cachedRepoSigners puts the TTL cache behind the RepoSignerSource seam —
// exactly the placement serviceauth.go's seam docs promised, with no caller
// change: getServiceAuth and the proxy path share it.
type cachedRepoSigners struct {
	inner atprotopds.RepoSignerSource
	ttl   time.Duration

	mu      sync.Mutex
	entries map[string]cachedSigner
}

type cachedSigner struct {
	signer  atprotopds.RepoSigner
	expires time.Time
}

func newCachedRepoSigners(inner atprotopds.RepoSignerSource) *cachedRepoSigners {
	return &cachedRepoSigners{
		inner:   inner,
		ttl:     repoSignerCacheTTL,
		entries: make(map[string]cachedSigner),
	}
}

func (c *cachedRepoSigners) RepoSigner(ctx context.Context, actorID []byte) (atprotopds.RepoSigner, error) {
	key := hex.EncodeToString(actorID)
	now := time.Now()

	c.mu.Lock()
	if e, ok := c.entries[key]; ok && now.Before(e.expires) {
		c.mu.Unlock()
		return e.signer, nil
	}
	c.mu.Unlock()

	// The unseal runs outside the lock — it is a nest round-trip, and a
	// concurrent duplicate unseal is cheaper than serializing every proxied
	// request behind one.
	signer, err := c.inner.RepoSigner(ctx, actorID)
	if err != nil {
		return nil, err
	}

	c.mu.Lock()
	for k, e := range c.entries {
		if !now.Before(e.expires) {
			delete(c.entries, k)
		}
	}
	c.entries[key] = cachedSigner{signer: signer, expires: now.Add(c.ttl)}
	c.mu.Unlock()
	return signer, nil
}
