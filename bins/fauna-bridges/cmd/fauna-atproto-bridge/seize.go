//go:build fauna_e2e_seize

// The hostile-rotation seam: one shot, e2e builds only.
//
// # Why this exists
//
// The ratified custody split (`docs/goal/behavior/atproto-pds-bridge.md` § State
// & data shape) lists the bridge's junior rotation key in the genesis op, and
// PLC accepts a next op from ANY listed key — so a compromised bridge can sign a
// `plc_operation` that installs a foreign key at `rotationKeys[0]` and seize the
// identity. The user's protection is not that this is impossible; it is that the
// user's key is strictly senior and can fork the seizure away inside PLC's 72 h
// window. Proving that remedy end to end therefore needs a *genuinely signed*
// seizure, and only two keys can produce one: the user's (client-only) and this
// bridge's. The e2e fake directory holds neither, and handing it either would be
// a far worse hole than the one finding closed. So the signer must be
// the bridge, which is what this file is.
//
// # Why a build tag rather than an env var
//
// Convention 15 (`docs/goal/architecture/testing.md` point 15): the automation
// surface is compiled out of release artifacts, and a runtime gate alone is not
// enough — it ships the capability inside every release binary, reachable by
// whoever controls the launch environment. A prior finding was
// exactly that mistake. The `FAUNA_ATPROTO_*` redirect seams elsewhere in this
// binary (`*_seam.go`, `-tags fauna_e2e_fixtures`) are gated the same way since
// 2026-09-13 — the permission-set chain made the DNS redirect its root of trust,
// so "redirects without adding a capability" stopped being true of them. `just
// atproto-bridge-build-e2e` is the only build that sets either tag, and the
// production recipe asserts its own artifact carries no trace of any seam.
//
// # Why it drives the production path
//
// Every step below is the same call the handle-rename hook makes
// (`projection.go`'s reconcileHandle): FetchLastOp → BuildUpdateOpFromPrev →
// Sign → SubmitOperation. A test-only re-implementation of op building would
// prove a property of itself; this proves a property of the shipped mechanism.
package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/keypair"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/logging"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

var (
	seizeDID         *string
	seizeRotationKey *string
)

func registerSeizeFlags(fs *flag.FlagSet) {
	seizeDID = fs.String("seize-did", "",
		"E2E-ONLY: sign and submit a hostile rotation for this did:plc, then exit. Models a compromised bridge for the recovery-fork contest test; requires --seize-rotation-key")
	seizeRotationKey = fs.String("seize-rotation-key", "",
		"E2E-ONLY: the foreign `did:key` to install at rotationKeys[0] by --seize-did")
}

// maybeRunSeize handles the run when --seize-did is set, reporting (true, err).
// It is deliberately a one-shot mode rather than a listener: it binds no port,
// so there is no surface to reach even in a build that has it, and the caller
// sees a real exit code instead of polling for a side effect.
func maybeRunSeize(ctx context.Context, keypairFile, nestEndpoint, logLevel string, out io.Writer) (bool, error) {
	if seizeDID == nil || *seizeDID == "" {
		return false, nil
	}
	if *seizeRotationKey == "" {
		return true, fmt.Errorf("--seize-rotation-key is required with --seize-did")
	}
	if err := logging.Init(logLevel, roleAtprotoPds); err != nil {
		return true, fmt.Errorf("init logger: %w", err)
	}
	logger := slog.Default()

	// The keyfile is already enrolled and approved — this runs beside a live
	// bridge that did that work. So the boot dance (announce, poll for
	// approval, whoami, register, fetch config) is skipped entirely: an
	// authenticated dial is all a key fetch needs.
	kf, err := keypair.LoadOrCreate(keypairFile, roleAtprotoPds, "unresolved-bridge")
	if err != nil {
		return true, fmt.Errorf("load keyfile: %w", err)
	}
	nestHTTP := wsrpc.NestHTTPClient(nestEndpoint, logger)
	dialCtx, cancel := context.WithTimeout(ctx, rpcDeadline)
	defer cancel()
	client, err := wsrpc.Dial(dialCtx, wsrpc.ClientConfig{
		NestEndpoint: nestEndpoint,
		AuthClient:   wsrpc.NewAuthClient(nestHTTP, nestEndpoint, kf.Ed25519PublicKey(), kf.SigningKey()),
		Logger:       logger,
		HTTPClient:   nestHTTP,
	})
	if err != nil {
		return true, fmt.Errorf("dial nest: %w", err)
	}
	defer client.Close()

	identities, err := wsrpc.FetchAtprotoIdentities(ctx, client)
	if err != nil {
		return true, fmt.Errorf("fetch identity roster: %w", err)
	}
	var target *wsrpc.AtprotoIdentityView
	for i := range identities {
		if identities[i].DID != nil && *identities[i].DID == *seizeDID {
			target = &identities[i]
			break
		}
	}
	if target == nil {
		return true, fmt.Errorf("no identity on the roster carries DID %s", *seizeDID)
	}

	x25519Secret := kf.X25519Secret()
	rotationKey, err := rotationKeyForActor(
		ctx, client, mailfauna.UnsealAtprotoIdentityBlob, x25519Secret[:], target.ActorID,
	)
	if err != nil {
		return true, fmt.Errorf("unseal bridge rotation key: %w", err)
	}

	httpClient := &http.Client{Timeout: 30 * time.Second}
	directoryBaseURL := atprotoid.PLCDirectoryBaseURL()
	prev, prevCID, err := atprotoid.FetchLastOp(ctx, httpClient, directoryBaseURL, *seizeDID)
	if err != nil {
		return true, fmt.Errorf("read plc log: %w", err)
	}

	op := atprotoid.BuildUpdateOpFromPrev(prev, prevCID, atprotoid.PrimaryHandle(prev))
	// BuildUpdateOpFromPrev is the RENAME builder: it rewrites alsoKnownAs from
	// the handle argument, which would collapse a multi-handle list to one entry.
	// A seizure changes the rotation keys and nothing else, so restore the
	// published list verbatim — a second, unannounced change would make the test
	// prove something other than what it claims.
	op.AlsoKnownAs = append([]string(nil), prev.AlsoKnownAs...)
	if len(op.RotationKeys) == 0 {
		return true, fmt.Errorf("standing head for %s lists no rotation keys", *seizeDID)
	}
	displaced := op.RotationKeys[0]
	op.RotationKeys[0] = *seizeRotationKey
	if err := op.Sign(rotationKey); err != nil {
		return true, fmt.Errorf("sign hostile rotation: %w", err)
	}
	signed, err := op.SignedCBOR()
	if err != nil {
		return true, err
	}
	cid, err := atprotoid.GenesisCid(signed)
	if err != nil {
		return true, fmt.Errorf("derive submitted op cid: %w", err)
	}
	if err := atprotoid.SubmitOperation(ctx, httpClient, directoryBaseURL, *seizeDID, op); err != nil {
		return true, fmt.Errorf("submit hostile rotation: %w", err)
	}
	logger.Warn("E2E SEIZURE: submitted a bridge-signed rotation displacing the senior key",
		"did", *seizeDID, "displaced", displaced, "installed", *seizeRotationKey, "cid", cid)
	// stdout is the test's channel — one line, the CID a user consent would be
	// scoped to.
	fmt.Fprintf(out, "%s\n", cid)
	return true, nil
}
