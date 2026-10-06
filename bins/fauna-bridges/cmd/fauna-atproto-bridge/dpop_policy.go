package main

// The production seat of the shared-Rust DPoP module
// (`fauna_bridge_atproto::dpop`) the PDS's resource server consults on every
// OAuth-plane call.
//
// Like ffiAuthorizer (projection.go), the adapter is **pure carriage**: no
// branch here may decide anything about a proof. See internal/atprotopds/dpop.go
// for the split between what the module decides and what Go does.

import (
	faunaAtproto "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_bridge_atproto"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
)

// ffiDPoPPolicy is the production atprotopds.DPoPPolicy seam.
type ffiDPoPPolicy struct{}

func (ffiDPoPPolicy) ValidateDPoPProof(compact string, expect atprotopds.DPoPExpectations) atprotopds.DPoPVerdict {
	v := faunaAtproto.ValidateDpopProof(compact, faunaAtproto.DpopExpectations{
		Htm:         expect.HTM,
		Htu:         expect.HTU,
		NowUnix:     expect.NowUnix,
		MaxAgeSecs:  expect.MaxAgeSecs,
		MaxSkewSecs: expect.MaxSkewSecs,
		ExpectedAth: expect.ExpectedAth,
	})
	switch t := v.(type) {
	case faunaAtproto.DpopVerdictValid:
		return atprotopds.DPoPVerdict{Proof: &atprotopds.DPoPProof{
			SigningInput: t.Proof.SigningInput,
			Signature:    t.Proof.Signature,
			PublicKeyX:   t.Proof.PublicKeyX,
			PublicKeyY:   t.Proof.PublicKeyY,
			JTI:          t.Proof.Jti,
			Nonce:        t.Proof.Nonce,
			IssuedAt:     t.Proof.IssuedAt,
		}}
	case faunaAtproto.DpopVerdictInvalid:
		return atprotopds.DPoPVerdict{Deny: &atprotopds.OAuthDeny{Error: t.Error, Description: t.Description}}
	default:
		return atprotopds.DPoPVerdict{Deny: unknownVariantDeny}
	}
}

// unknownVariantDeny is the closed-world answer to a verdict shape this build
// does not know. `server_error` rather than a client-blaming code: the fault is
// ours, and telling a caller its proof was invalid when our own seam is out of
// step would send a client author chasing a bug they do not have.
var unknownVariantDeny = &atprotopds.OAuthDeny{
	Error:       "server_error",
	Description: "DPoP policy returned an unrecognized verdict",
}
