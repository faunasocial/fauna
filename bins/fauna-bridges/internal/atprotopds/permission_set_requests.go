package atprotopds

// The bridge's half of the permission-set request call: the nest's own
// `/oauth/par` cannot resolve an `include:<NSID>` — the chain (TXT → DID →
// proof) lives here and stays here — so it pushes
// `fauna.bridges.atproto.permission_set_requested` and this file answers with
// `fauna.bridges.atproto.deliver_permission_set` (atproto-oauth-provider.md
// § Implementation status today, the 2026-09-25 bullet).
//
// Two rules hold on this path: the bytes cross verbatim (expansion is the
// shared module's, nest-side), and a refusal's reason stays in this log.

import (
	"context"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// SetDocumentResolver resolves a permission set's NSID to the **verified**
// dag-cbor bytes of its published `com.atproto.lexicon.schema` record.
//
// Production wires `internal/atprotolex`'s chain (DNS TXT → DID → the DID
// document's PDS and signing key → `sync.getRecord` through the SSRF guard →
// MST-proof and commit-signature verification), behind its document cache. The
// seam exists so this package stays cgo-free and so a test can drive the refusal
// arms without a network.
type SetDocumentResolver interface {
	ResolveSetDocument(ctx context.Context, nsid string) ([]byte, error)
}

// EnablePermissionSets attaches the resolution chain the nest's `/oauth/par`
// asks this bridge to run (`atproto-pds-full.md:332`). Without it every
// request is answered with a refusal — the fail-closed baseline, not a
// degraded mode to paper over.
func (s *Server) EnablePermissionSets(resolver SetDocumentResolver) {
	s.setResolver = resolver
}

// permissionSetDeliveryBudget bounds one resolution plus its delivery.
//
// The nest holds a PAR for thirty seconds on the other end
// (`PERMISSION_SET_RESOLVE_TIMEOUT`); a resolution still running past that
// horizon answers nobody, so the work is cut at the same one rather than left
// to the chain's own per-leg timeouts to add up.
const permissionSetDeliveryBudget = 30 * time.Second

// HandlePermissionSetRequested answers one
// `fauna.bridges.atproto.permission_set_requested` push.
//
// It runs the resolution OFF the calling goroutine. Pushes are dispatched on
// the WS client's read loop, and the answer is itself a Call whose reply that
// same loop must deliver — resolving inline would hold the loop through three
// network legs and then wait on a reply the held loop can never read.
func (s *Server) HandlePermissionSetRequested(ctx context.Context, requestID []byte, nsid string) {
	go s.deliverPermissionSet(ctx, requestID, nsid)
}

// deliverPermissionSet is the synchronous body of HandlePermissionSetRequested:
// resolve, then deliver the verified bytes — or a refusal, as the absence of a
// record — under the request id the nest is waiting on.
func (s *Server) deliverPermissionSet(ctx context.Context, requestID []byte, nsid string) {
	ctx, cancel := context.WithTimeout(ctx, permissionSetDeliveryBudget)
	defer cancel()

	var record []byte
	switch {
	case s.setResolver == nil:
		// No resolution plane wired on this bridge: the honest answer is the
		// refusal, delivered now so the nest does not wait out its deadline
		// for an answer that will never come.
		s.logger.Info("atproto oauth: permission set requested by the nest, but no resolution plane is wired; refusing",
			"nsid", nsid)
	default:
		doc, err := s.setResolver.ResolveSetDocument(ctx, nsid)
		if err != nil {
			// Logged, never carried — the reason describes this deployment's
			// outbound network to whoever chose the NSID.
			s.logger.Info("atproto oauth: permission set resolution for the nest failed",
				"nsid", nsid, "err", err)
		} else {
			record = doc
		}
	}

	accepted, err := wsrpc.DeliverAtprotoPermissionSet(ctx, s.nest, requestID, nsid, record)
	if err != nil {
		s.logger.Warn("atproto oauth: delivering a permission set to the nest failed",
			"nsid", nsid, "err", err)
		return
	}
	if !accepted {
		// Ordinary: the nest's deadline refused first, or the waiter was shed.
		s.logger.Info("atproto oauth: the nest no longer waits on this permission set (late answer)",
			"nsid", nsid)
	}
}
