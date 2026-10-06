package atprotopds

// D8 — the bridge's side of the single per-request authorization decision
// point (docs/goal/behavior/atproto-pds-full.md § F3 detail, the *Ratified
// contract* block).
//
// There is deliberately NO authorization logic in this file. The decision
// lives in shared Rust (`fauna_bridge_atproto::authz`), reached over the
// tracked Go binding; Go's whole job is to assemble the module's input from
// the route and the verified caller, and to enforce whatever verdict comes
// back. That is what makes F4's OAuth plane a second *caller* rather than a
// second enforcement path — and why a new lexicon or scope is a Rust change
// with Rust tests, never a Go change.
//
// The seam below keeps this package free of cgo (the `ffiTranslator` pattern
// the projection loop already uses): production wires the FFI adapter from
// cmd/fauna-atproto-bridge, tests wire a fake.

import (
	"net/http"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// AuthzInput mirrors `fauna_bridge_atproto::authz::AuthzInput`. Pure data
// carriage across the seam — every field is something the XRPC frame already
// carries.
type AuthzInput struct {
	// Plane is xrpc.Caller.Plane verbatim.
	Plane string
	// Scopes the presented credential carries. OAuth's `scope` claim is
	// space-delimited by spec, so splitting on whitespace serves both planes:
	// the app plane yields exactly one element, an OAuth grant yields its
	// granular set.
	Scopes []string
	// ExternalAppsEnabled is the account's kill-switch as the bridge last
	// learned it — an INPUT to the module now, no longer a check of its own.
	ExternalAppsEnabled bool
	// Lxm is the method being authorized (the route's NSID).
	Lxm string
	// Aud is the proxy-target service DID, nil off the proxy path.
	Aud *string
	// EndpointClass is the route's declared class, cross-language name.
	EndpointClass string
}

// AuthzVerdict mirrors `fauna_bridge_atproto::authz::AuthzVerdict`.
type AuthzVerdict struct {
	Allow bool
	// XrpcError is the wire error name when !Allow. It carries the D6 refusal
	// sub-type; see xrpc.DenyFromModule.
	XrpcError string
	Message   string
}

// Authorizer is the D8 seam — the shared-Rust module.
type Authorizer interface {
	Authorize(AuthzInput) AuthzVerdict
}

// AuthzHook is D8's seat in the frame's middleware chain (xrpc.AuthzHook), run
// post-authentication and pre-handler. It replaced F1's direct kill-switch
// check: one decision point, never two.
func (s *Server) AuthzHook(r *http.Request, route *xrpc.Route, caller *xrpc.Caller) *xrpc.Error {
	if caller == nil {
		// A Public route. `Auth: Public` in the route table IS its
		// authorization — there is no plane, so there is nothing for the
		// module to decide.
		return nil
	}
	if s.authz == nil {
		// Closed world extends to the seam itself: with no module wired, no
		// decision can be made, so none is assumed.
		return xrpc.AuthRequired()
	}
	return s.decide(s.assemble(caller, route.NSID, s.proxyAud(r, route), route.Class))
}

// assemble builds the module's input. ONE assembly site by design: a second
// would be how the two getServiceAuth checks silently drift apart.
func (s *Server) assemble(caller *xrpc.Caller, lxm string, aud *string, class xrpc.EndpointClass) AuthzInput {
	return AuthzInput{
		Plane: caller.Plane,
		// OAuth's `scope` claim is space-delimited by spec, so one splitter
		// serves both planes: the app plane yields exactly one element.
		Scopes: strings.Fields(caller.Scope),
		// The flag cache survives F1 purely as this input's source.
		ExternalAppsEnabled: s.flags.enabled(caller.ActorID),
		Lxm:                 lxm,
		Aud:                 aud,
		EndpointClass:       class.String(),
	}
}

// decide runs the module and puts its verdict on the wire. No branch here may
// ever depend on WHAT was refused — only that it was.
func (s *Server) decide(in AuthzInput) *xrpc.Error {
	v := s.authz.Authorize(in)
	if v.Allow {
		return nil
	}
	return xrpc.DenyFromModule(v.XrpcError, v.Message)
}

// AuthorizeServiceAuth is the second of the two checks a `getServiceAuth` call
// runs (atproto-pds-full.md `:193`: the route NSID *and* the requested lxm are
// both checked). The first is the ordinary hook above — may this credential
// mint at all; this one asks whether it may mint for the method and audience
// it actually requested, through the same matrix. Two checks, one decision
// function: that is how migration-oriented minting stays deferred-refused
// without a second code path to keep in sync.
func (s *Server) AuthorizeServiceAuth(caller *xrpc.Caller, lxm, aud string) *xrpc.Error {
	if caller == nil || s.authz == nil {
		return xrpc.AuthRequired()
	}
	return s.decide(s.assemble(caller, lxm, &aud, xrpc.ClassAuthed))
}

// proxyAud reads the service DID this request would be forwarded to.
//
// The target ref IS the audience: resolution turns the DID into an endpoint
// URL, but the service-auth `aud` claim carries the DID itself. Consulted only
// on routes the frame declares Proxyable — the header is attacker-controlled,
// and on a non-proxy route it has no business reaching the decision. (Even
// where it is read it cannot widen a grant: naming the chat service only ever
// *adds* the privileged-scope requirement, and an OAuth `rpc:…?aud=` scope
// that matches a forged header is a scope for the service the request would
// then actually be sent to.)
//
// Delegating to proxyTarget (proxy.go) is what makes the headerless AppView
// default reach D8: the module authorizes against the same audience the
// forward dials, one computation for both.
func (s *Server) proxyAud(r *http.Request, route *xrpc.Route) *string {
	if r == nil || route == nil || !route.Proxyable {
		return nil
	}
	if t, ok := s.proxyTarget(r, route.NSID); ok {
		return &t
	}
	return nil
}
