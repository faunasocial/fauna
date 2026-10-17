package atprotopds

// The bridge's half of the oracle's `atproto.service_auth` class
// (key-material-hierarchy.md § The oracle → *The bridge-held custodian*). A
// third-party principal asked the NEST's door `fauna.atproto.oracle.service_auth`
// for a service JWT; the door decided — the principal's live grant, its
// declared (aud, lxm) set, the deny half, the ceiling — and pushes
// `fauna.bridges.atproto.service_auth_requested` here, because the repo signing
// key is sealed to this bridge and nowhere else. This file mints and answers
// over `fauna.bridges.atproto.deliver_service_auth`.
//
// The bridge adds no policy of its own: the push arrived over its
// authenticated nest connection, and the nest door IS the decision (so D8's
// AuthorizeServiceAuth, which judges an ATProto session's credential, has
// nothing to judge here). It mints with the same signer and claim set as
// getServiceAuth, clamping the lifetime to the one shared cap again.

import (
	"context"
	"errors"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// errNoSignerSource is the refusal when no repo signer source is wired.
var errNoSignerSource = errors.New("no repo signer source wired")

// ServiceAuthPush is one decoded `service_auth_requested` push.
type ServiceAuthPush struct {
	RequestID []byte
	ActorID   []byte
	// DID is the token's `iss` — the account's active DID as the nest
	// resolved it; the bridge never reads identity status nor invents one.
	DID string
	Aud string
	Lxm string
	// Exp is the absolute expiry (unix seconds) the nest already clamped.
	Exp int64
}

// serviceAuthDeliveryBudget bounds one mint plus its delivery — the nest's
// own wait (`SERVICE_AUTH_RESOLVE_TIMEOUT`), past which an answer reaches
// nobody.
const serviceAuthDeliveryBudget = 10 * time.Second

// HandleServiceAuthRequested answers one push. It mints OFF the calling
// goroutine: pushes are dispatched on the WS client's read loop, and the
// signer's cold path is itself a nest Call whose reply that loop must read.
func (s *Server) HandleServiceAuthRequested(ctx context.Context, push ServiceAuthPush) {
	go s.deliverServiceAuth(ctx, push)
}

// deliverServiceAuth is the synchronous body of HandleServiceAuthRequested:
// mint, then deliver the token — or a refusal, as its absence — under the
// request id the nest is waiting on.
func (s *Server) deliverServiceAuth(ctx context.Context, push ServiceAuthPush) {
	ctx, cancel := context.WithTimeout(ctx, serviceAuthDeliveryBudget)
	defer cancel()

	var token *string
	if minted, err := s.mintForNest(ctx, push); err != nil {
		// Logged, never carried — the detail describes bridge-internal key
		// state.
		s.logger.Warn("atproto oracle: could not mint the service token the nest asked for",
			"aud", push.Aud, "lxm", push.Lxm, "err", err)
	} else {
		token = &minted
	}

	accepted, err := wsrpc.DeliverAtprotoServiceAuth(ctx, s.nest, push.RequestID, token)
	if err != nil {
		s.logger.Warn("atproto oracle: delivering a service token to the nest failed",
			"aud", push.Aud, "lxm", push.Lxm, "err", err)
		return
	}
	if !accepted {
		// Ordinary: the nest's deadline refused first.
		s.logger.Info("atproto oracle: the nest no longer waits on this service token (late answer)",
			"aud", push.Aud, "lxm", push.Lxm)
	}
}

// mintForNest resolves the account's signer and mints. MintServiceAuth
// refuses an incomplete claim set itself, so an empty DID, aud or lxm on the
// push can never become an unbound token.
func (s *Server) mintForNest(ctx context.Context, push ServiceAuthPush) (string, error) {
	if s.signers == nil {
		return "", errNoSignerSource
	}
	signer, err := s.signers.RepoSigner(ctx, push.ActorID)
	if err != nil {
		return "", err
	}
	// One clock reading for both the lifetime arithmetic and the claim.
	now := time.Now()
	return MintServiceAuth(signer, ServiceAuthRequest{
		Iss:      push.DID,
		Aud:      push.Aud,
		Lxm:      push.Lxm,
		Lifetime: time.Unix(push.Exp, 0).Sub(now),
	}, now)
}
