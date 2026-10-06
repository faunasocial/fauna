// The S2 DID mint loop: poll nest's per-user ATProto identity roster and mint
// a DID for every row still "pending" (atproto-pds-bridge.md § Identity).
//
//   - method "plc": fetch + unseal the bridge-custodied key blob, build the
//     genesis `plc_operation` with the USER-custodied rotation key at
//     rotationKeys[0] (the ratified seniority invariant — the bridge key is
//     junior at index 1), sign with the bridge rotation key, derive the
//     did:plc + genesis CID, submit to the PLC directory, report back via
//     record_minted_identity.
//   - method "web": no directory — the DID is `did:web:<handle>`; report it.
//
// Every per-identity failure is logged and retried on the next poll
// (record_minted_identity is idempotent on the same DID; PLC genesis submits
// re-derive the same DID from the same op, and nest returns the same sealed
// key blob on every fetch, so retries are safe end-to-end). After a
// successful record the resolvability self-check runs WARN-ONLY (S2 builds
// the primitive; S3/S5 wire it as the pre-firehose gate).
package main

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// mintPollInterval is how often the bridge re-reads the identity roster. A
// hard-coded cadence, not a knob: nobody chooses it (product invariant — the
// only configuration surface is the apps).
const mintPollInterval = 30 * time.Second

// mintDeps carries the mint loop's injectable seams. Production wiring comes
// from newMintDeps; tests substitute the unseal fn, directory URL, HTTP
// client, and resolver.
type mintDeps struct {
	// x25519Secret is the bridge's 32-byte service-user X25519 private key —
	// the recipient secret the identity key blobs are sealed to.
	x25519Secret []byte
	// unseal opens a sealed AtprotoIdentityBlob (production:
	// mailfauna.UnsealAtprotoIdentityBlob over the FFI).
	unseal unsealIdentityBlobFn
	// directoryBaseURL is the PLC directory (production: the hard-coded
	// default, or the test-only env seam — atprotoid.PLCDirectoryBaseURL).
	directoryBaseURL string
	httpClient       *http.Client
	resolver         atprotoid.TXTResolver
}

func newMintDeps(x25519Secret []byte) *mintDeps {
	return &mintDeps{
		x25519Secret:     x25519Secret,
		unseal:           mailfauna.UnsealAtprotoIdentityBlob,
		directoryBaseURL: atprotoid.PLCDirectoryBaseURL(),
		httpClient:       &http.Client{Timeout: 30 * time.Second},
		resolver:         net.DefaultResolver,
	}
}

// runMintPass performs one roster read + mint sweep. Roster-read failures are
// logged and left for the next poll (transient WS blips are the reconnector's
// business); per-identity failures never stop the sweep.
func runMintPass(ctx context.Context, c wsrpc.Caller, deps *mintDeps, logger *slog.Logger) {
	identities, err := wsrpc.FetchAtprotoIdentities(ctx, c)
	if err != nil {
		logger.Warn("atproto identity roster fetch failed (retrying next poll)", "err", err)
		return
	}
	for _, id := range identities {
		if id.Status != wsrpc.IdentityStatusPending {
			continue
		}
		did, err := mintIdentity(ctx, c, deps, id)
		if err != nil {
			logger.Warn("atproto identity mint failed (retrying next poll)",
				"handle", id.Handle, "method", id.Method, "err", err)
			continue
		}
		logger.Info("atproto identity minted", "handle", id.Handle, "method", id.Method, "did", did)

		// Warn-only first-impression self-check: an identity that does not
		// resolve at first AppView index can permanently 404 ecosystem-side,
		// so surface a broken deployment loudly — but the mint is recorded
		// and must not fail on it.
		if err := atprotoid.VerifyIdentityResolvable(
			ctx, deps.resolver, deps.httpClient, deps.directoryBaseURL, did, id.Handle,
		); err != nil {
			logger.Warn("atproto identity not (yet) resolvable — Bluesky indexing may fail until this clears",
				"handle", id.Handle, "did", did, "err", err)
		}
	}
}

// mintIdentity mints one pending identity and reports the DID to nest.
func mintIdentity(ctx context.Context, c wsrpc.Caller, deps *mintDeps, id wsrpc.AtprotoIdentityView) (string, error) {
	switch id.Method {
	case "web":
		did := atprotoid.DIDWeb(id.Handle)
		if err := wsrpc.RecordMintedIdentity(ctx, c, id.ActorID, did, nil); err != nil {
			return "", err
		}
		return did, nil
	case "plc":
		return mintPlcIdentity(ctx, c, deps, id)
	default:
		return "", fmt.Errorf("unknown DID method %q", id.Method)
	}
}

func mintPlcIdentity(ctx context.Context, c wsrpc.Caller, deps *mintDeps, id wsrpc.AtprotoIdentityView) (string, error) {
	// The USER-custodied senior rotation pubkey is minted client-side and must
	// already be on the roster row — without it there is nothing to place at
	// rotationKeys[0], and minting with the bridge key alone would violate the
	// custody invariant (nest/bridge never sole custodian).
	if id.UserRotationPubDIDKey == "" {
		return "", fmt.Errorf("plc identity has no user rotation pubkey yet (client has not contributed its key)")
	}

	// Genesis is the one place a wrong blob does LASTING harm — the keys inside
	// are published into a brand-new DID document, where a later signature with
	// the wrong key merely verifies nowhere. openIdentityKeys refuses a blob
	// whose keys are not the ones this identity's row publishes.
	bundle, err := openIdentityKeys(ctx, c, deps.unseal, deps.x25519Secret, id.ActorID)
	if err != nil {
		return "", err
	}
	// The unsealed scalars are secrets: overwrite them the moment the mint is
	// done (short-lifetime discipline; no on-disk copies).
	defer zeroize(bundle.SigningPriv)
	defer zeroize(bundle.RotationPriv)

	if bundle.RotationCurve != "k256" {
		return "", fmt.Errorf("unsupported rotation curve %q (want k256)", bundle.RotationCurve)
	}
	rotationKey, err := atprotoid.PrivateKeyFromK256Scalar(bundle.RotationPriv)
	if err != nil {
		return "", fmt.Errorf("bridge rotation key from scalar: %w", err)
	}
	// Self-check: the scalar we will sign with must be the public key nest
	// recorded — a mismatch means seal/provision drift and the signature
	// would never verify ecosystem-side.
	derivedDIDKey, err := atprotoid.DIDKeyForPrivate(rotationKey)
	if err != nil {
		return "", fmt.Errorf("derive rotation did:key: %w", err)
	}
	if derivedDIDKey != bundle.RotationPubDidKey {
		return "", fmt.Errorf("unsealed rotation scalar derives %s but nest recorded %s (seal drift)",
			derivedDIDKey, bundle.RotationPubDidKey)
	}

	op := atprotoid.BuildGenesisOp(
		id.UserRotationPubDIDKey, // rotationKeys[0] — USER key, most senior.
		bundle.RotationPubDidKey, // rotationKeys[1] — bridge junior key.
		bundle.SigningPubDidKey,
		id.Handle,
		id.PDSEndpoint,
	)
	if err := op.Sign(rotationKey); err != nil {
		return "", err
	}
	signed, err := op.SignedCBOR()
	if err != nil {
		return "", err
	}
	did := atprotoid.DerivePlcDid(signed)
	genesisCID, err := atprotoid.GenesisCid(signed)
	if err != nil {
		return "", err
	}

	if err := atprotoid.SubmitOperation(ctx, deps.httpClient, deps.directoryBaseURL, did, op); err != nil {
		return "", err
	}
	if err := wsrpc.RecordMintedIdentity(ctx, c, id.ActorID, did, &genesisCID); err != nil {
		return "", err
	}
	return did, nil
}

// zeroize overwrites a secret byte slice in place.
func zeroize(b []byte) {
	for i := range b {
		b[i] = 0
	}
}
