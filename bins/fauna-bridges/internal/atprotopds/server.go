package atprotopds

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"log/slog"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/auth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// rpcTimeout caps each nest round-trip inside an XRPC handler (the
// mail-bridge AUTH convention, internal/auth.RPCTimeout).
const rpcTimeout = auth.RPCTimeout

// createSession's per-identifier failure lockout: 10 failures per 5-minute
// window at the fine bucket, with authlock's built-in coarse (identifier ×
// all-IPs, IP × all-identifiers) escalation multipliers. Hard-coded per the
// product invariant; the per-IP × endpoint-class window is the xrpc
// frame's separate layer. (The design sketch says "exponential backoff";
// authlock's reviewed three-tier fixed-window lockout is the shipped
// primitive mail already uses — F1 reuses it, tuning happens before ship.)
const (
	lockoutLimit  = 10
	lockoutWindow = 5 * time.Minute
)

// The per-account DID is the account's REAL ATProto DID, resolved nest-side
// and carried in the session token's `sub` (slice 4d). It is never derived
// here: F1 shipped a `did:fauna:<hex>` placeholder computed from the account
// pubkey, and because the projection loop always committed under the real
// did:plc, the two halves of the bridge maintained two different repos for one
// account. Both directions of that derivation are gone — the DID enters at
// createSession from `login_did` and is carried, and the actor id rides its own
// `fauna_actor` claim (token.go) rather than being decoded back out of a DID.
//
// The rule, for anything added here later: a DID is DATA (internal/wsrpc's
// AtprotoIdentityView.DID says it too — "key everything off this value, never
// re-derive it"). If you find yourself computing one, you are reintroducing 4d.

// dummyVerifier is a production-cost Argon2id PHC string used to equalize
// createSession timing when the identifier resolves to no account / no
// credentials: the handler burns one real verify against it so an
// attacker cannot distinguish "unknown user" from "wrong secret" by
// response time. (Minted once from a throwaway secret; the secret is not
// retained anywhere.)
const dummyVerifier = "$argon2id$v=19$m=65536,t=2,p=1$eNjjD+aAhbx15zk/vc1hhQ$kqWqTgBjdvibBY7cOH6+Q9wGKfj2RP8akBJSpzaaaYY"

// flagCache is the per-account external-apps kill-switch cache
// (atproto-pds-full.md § F1 detail: the bridge rejects even valid access
// tokens per-request via this cached flag, refreshed by the
// sessions_changed nudge). Population: createSession (the flag rides the
// verifier fetch) and nudges carrying Some(new value). Missing entry =
// enabled (the default-ON semantics); after a bridge restart a stale
// unexpired access token of a disabled account can therefore pass again
// until it expires — inside the design's stated 60-min revocation bound
// (refresh is refused nest-side regardless).
//
// Since F3 this cache is NOT a check of its own: it is the source of D8's
// `external_apps_enabled` input (authz.go). The enforcement moved into the
// shared-Rust module — one decision point, never two.
type flagCache struct {
	mu       sync.Mutex
	disabled map[string]bool // actor hex → true when kill-switch OFF
}

func newFlagCache() *flagCache {
	return &flagCache{disabled: make(map[string]bool)}
}

func (f *flagCache) enabled(actorID []byte) bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	return !f.disabled[hex.EncodeToString(actorID)]
}

func (f *flagCache) set(actorID []byte, enabled bool) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if enabled {
		delete(f.disabled, hex.EncodeToString(actorID))
	} else {
		f.disabled[hex.EncodeToString(actorID)] = true
	}
}

// Server implements the F1 com.atproto.server.* surface on top of the
// xrpc frame, the nest session registry, and the HS256 token minter.
type Server struct {
	nest   wsrpc.Caller
	minter *TokenMinter
	lock   *authlock.Lockout
	flags  *flagCache
	// authz is D8, the shared-Rust decision module (authz.go). A nil module
	// refuses every authenticated call rather than allowing it.
	authz Authorizer
	// signers resolves a per-account repo signing key for service-auth
	// minting (serviceauth.go). Nil refuses the mint rather than serving it.
	signers RepoSignerSource
	// proxy is the F3 service-proxy wiring (proxy.go). Nil = the proxy path
	// is off: ProxyFallback matches nothing and headerless defaults do not
	// apply.
	proxy *ProxyConfig
	// writer is the F2 repo-write seam (repo_write.go). Nil = the write
	// surface refuses rather than committing what it cannot persist.
	writer RepoWriter
	// blobs is the F2.4 blob-ingest seam (blob_upload.go). Nil = uploadBlob
	// refuses rather than accepting bytes it cannot persist.
	blobs BlobIngester
	// nestIssuerKeys is the NEST's OAuth issuer key set as this resource server
	// holds it (oauth_issuer_keys.go): the public keys a nest-minted access
	// token may be verified against, and the issuer identifier pinned when it
	// is. Fed over WS-RPC and replaced whole on every read, so a key the nest
	// has stopped serving stops verifying here. Nil, or holding an empty
	// issuer, = this deployment honours no OAuth token — a domainless nest has
	// no issuer to name.
	nestIssuerKeys atomic.Pointer[nestIssuerKeySet]
	// issuerKeyNudge asks the boot wiring's refresh loop to re-read that set.
	// Written non-blockingly from the verify path on an unknown `kid`; nil
	// outside a wired process (tests, dry runs).
	issuerKeyNudge chan<- struct{}
	// lastIssuerMissRefetch damps that nudge to one per
	// nestIssuerMissRefetchInterval — a stranger chooses the `kid` they
	// present, so an undamped miss path is an amplifier pointed at the nest.
	lastIssuerMissRefetch atomic.Int64
	// dpopPolicy is the shared-Rust DPoP module (oauth_resource_server.go).
	// Nil = the OAuth plane refuses every token rather than accepting a proof
	// nothing has judged.
	dpopPolicy DPoPPolicy
	// setResolver resolves a permission set's published Lexicon document for
	// the nest's own PAR (permission_set_requests.go). Nil is a live
	// configuration, not a broken one: every set the nest asks about is then
	// refused, which is the fail-closed baseline.
	setResolver SetDocumentResolver
	// dpopNonces issues and recognises the server nonces every DPoP proof must
	// carry, and dpopReplays remembers the proofs already spent (dpop.go). Both
	// bridge-memory and transient by design (C6): a restart costs a client one
	// `use_dpop_nonce` round trip, nothing more.
	dpopNonces  *dpopNonceMinter
	dpopReplays *replaySet
	// pdsOrigin is this PDS's own origin (`https://pds.<apex>`) — the prefix a
	// resource-server DPoP proof's `htu` is built from, per request path.
	// Derived from the shared-Rust origin builder, never from a request's own
	// Host header: `htu` is compared for equality, so taking it from the
	// request would let a caller reaching this process under any other name
	// satisfy the check against a URL we never published.
	pdsOrigin string
	// oauthClock is the clock the OAuth plane reads — token expiry, nonce
	// windows, the replay set, the miss-refetch damper — injectable so a TTL
	// test advances a clock instead of sleeping for one.
	oauthClock func() time.Time
	logger     *slog.Logger
}

// NewServer builds the PDS auth surface. nest is the authenticated WS-RPC
// caller (the reconnecting client in production; a fake in tests); authz is
// the shared-Rust D8 module (the FFI adapter in production; a fake in tests);
// signers resolves the per-account repo signing key service auth is minted
// with (the sealed-blob unseal in production; a fixed key in tests).
func NewServer(nest wsrpc.Caller, minter *TokenMinter, authz Authorizer, signers RepoSignerSource, logger *slog.Logger) *Server {
	if logger == nil {
		logger = slog.Default()
	}
	return &Server{
		nest:    nest,
		minter:  minter,
		lock:    authlock.New(lockoutLimit, lockoutWindow, nil),
		flags:   newFlagCache(),
		authz:   authz,
		signers: signers,
		// The real clock; in-package tests replace it before wiring the OAuth
		// plane, so every store built from it reads the test's clock.
		oauthClock: time.Now,
		logger:     logger,
	}
}

// HandleSessionsChanged applies a fauna.bridges.atproto.sessions_changed
// nudge: a kill-switch flip carries the new flag value; a revoke nudge
// (nil) just means registry state changed — nothing cached here to drop
// beyond the flag, since access tokens are registry-checked only at
// refresh (their 60-min lifetime is the revocation bound, per design).
func (s *Server) HandleSessionsChanged(actorID []byte, externalAppsEnabled *bool) {
	if externalAppsEnabled != nil {
		s.flags.set(actorID, *externalAppsEnabled)
	}
}

// VerifyAccess implements xrpc.TokenVerifier for the app-credential plane.
//
// The request is unused here by design: an app-credential token is a bearer
// credential (D3 rung 1), so nothing about the request under it changes whether
// it is valid. The OAuth plane's verifier does use it — see
// [Server.OAuthTokenVerifier].
func (s *Server) VerifyAccess(_ *http.Request, token string) (*xrpc.Caller, error) {
	c, err := s.minter.VerifyAccessScope(token)
	if err != nil {
		return nil, err
	}
	actor, err := c.ActorBytes()
	if err != nil {
		return nil, err
	}
	sid, err := c.SidBytes()
	if err != nil {
		return nil, err
	}
	return &xrpc.Caller{
		ActorID:   actor,
		DID:       c.Sub,
		Handle:    c.Handle,
		Scope:     c.Scope,
		SessionID: sid,
		Plane:     PlaneAppCredential,
	}, nil
}

// RegisterRoutes adds the F1 com.atproto.server.* routes to the shared
// route table (the interlock artifact — the mirror chain's S3 registers
// its read surface on the same server).
func (s *Server) RegisterRoutes(x *xrpc.Server) {
	x.Register(xrpc.Route{
		NSID: "com.atproto.server.describeServer", Method: http.MethodGet,
		Auth: xrpc.Public, Class: xrpc.ClassPublicRead, Handle: s.describeServer,
	})
	x.Register(xrpc.Route{
		NSID: "com.atproto.server.createSession", Method: http.MethodPost,
		Auth: xrpc.Public, Class: xrpc.ClassAuth, Handle: s.createSession,
	})
	// refreshSession/deleteSession present the REFRESH token as bearer —
	// the frame's access-token verifier must not run, so they register
	// Public and verify the refresh token handler-side.
	x.Register(xrpc.Route{
		NSID: "com.atproto.server.refreshSession", Method: http.MethodPost,
		Auth: xrpc.Public, Class: xrpc.ClassAuth, Handle: s.refreshSession,
	})
	x.Register(xrpc.Route{
		NSID: "com.atproto.server.deleteSession", Method: http.MethodPost,
		Auth: xrpc.Public, Class: xrpc.ClassAuth, Handle: s.deleteSession,
	})
	// `Session` (either plane), not `AppSession` — corrected in F4 slice 7, and
	// the correction is the reason the base OAuth scope exists at all.
	//
	// `atproto` grants the `SessionIdentity` class precisely so a client can ask
	// *who it authenticated as*: a client that could authenticate but not answer
	// that is broken (the scope model's own two-sided narrowness argument). This
	// route was registered `AppSession` when the app plane was the only one, and
	// that became wrong the moment an OAuth token could exist — it made the base
	// scope unusable against the one method it is defined to cover.
	//
	// ⚠ Found by the tier_3 four-process run, not by any unit test, and that is
	// the general shape: a route's auth CLASS is a claim about which planes may
	// reach it, and every plane-level check below it (D8's matrix, the scope
	// model) is unreachable on a route whose class excludes the plane. When a
	// new plane lands, every route's class is a decision to re-make.
	x.Register(xrpc.Route{
		NSID: "com.atproto.server.getSession", Method: http.MethodGet,
		Auth: xrpc.Session, Class: xrpc.ClassAuthed, Handle: s.getSession,
	})
	// getServiceAuth mints with the CALLER's own repo signing key, so it is
	// authenticated and deliberately NOT Proxyable: the audience it mints for
	// comes from the query, and letting an `atproto-proxy` header also reach
	// D8 here would put two different audiences in play for one request.
	x.Register(xrpc.Route{
		NSID: "com.atproto.server.getServiceAuth", Method: http.MethodGet,
		Auth: xrpc.Session, Class: xrpc.ClassAuthed, Handle: s.getServiceAuth,
	})
	// Preferences (F3 phase 4) are served LOCALLY from nest state — PDS-private
	// per-account data (D4 custody), never forwarded. Registering them shadows
	// the headerless-AppView proxy fallback (registered routes always win), and
	// they are deliberately NOT Proxyable: a forged `atproto-proxy` header on
	// these must not divert a client's own private preferences to a third party.
	x.Register(xrpc.Route{
		NSID: "app.bsky.actor.getPreferences", Method: http.MethodGet,
		Auth: xrpc.Session, Class: xrpc.ClassAuthed, Handle: s.getPreferences,
	})
	x.Register(xrpc.Route{
		NSID: "app.bsky.actor.putPreferences", Method: http.MethodPost,
		Auth: xrpc.Session, Class: xrpc.ClassAuthed, Handle: s.putPreferences,
	})
	// F2 — com.atproto.repo.{createRecord,deleteRecord} (repo_write.go).
	s.registerWriteRoutes(x)
	// F2.4 — com.atproto.repo.uploadBlob (blob_upload.go).
	s.registerBlobRoutes(x)
}

// ── Handlers ────────────────────────────────────────────────────

func (s *Server) describeServer(w http.ResponseWriter, _ *http.Request, _ *xrpc.Caller) {
	// Static (F1 detail): accounts exist only via Fauna onboarding — no
	// invite codes, no email verification, no available user domains.
	xrpc.WriteJSON(w, map[string]any{
		"did":                  s.minter.serviceDID,
		"availableUserDomains": []string{},
	})
}

type createSessionRequest struct {
	Identifier string `json:"identifier"`
	Password   string `json:"password"`
}

func (s *Server) createSession(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	var req createSessionRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 16*1024)).Decode(&req); err != nil ||
		req.Identifier == "" || req.Password == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("expected identifier and password"))
		return
	}
	// The same peer notion the frame's rate limiter keys on — one extraction
	// for the whole listener (xrpc.ClientIP).
	ip := xrpc.ClientIP(r)
	identifier := strings.TrimSpace(req.Identifier)
	if s.lock.IsLockedFor(identifier, "", ip) {
		xrpc.WriteError(w, xrpc.RateLimited())
		return
	}

	ctx, cancel := timeoutCtx(r, rpcTimeout)
	defer cancel()
	acct, err := wsrpc.FetchAppCredentialVerifiers(ctx, s.nest, identifier)
	if err != nil {
		s.logger.Warn("createSession: verifier fetch failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	// Uniform failure: unknown identifier, disabled account, no
	// credentials, and wrong secret are indistinguishable in both response
	// and (via the dummy verify) timing.
	fail := func() {
		s.lock.RecordFailureFor(identifier, "", ip)
		xrpc.WriteError(w, xrpc.AuthRequired())
	}
	// `login_did` nil joins this set (slice 4d): the account has no ACTIVE
	// hosted ATProto identity, so there is no repo to serve, nothing for
	// getSession to name, and no `iss` service auth could mint. The nest holds
	// that rule in one place — the bridge does not read identity status and
	// must not invent a DID to log the account in as. It also makes the
	// ratified step-down suspension real (`atproto-pds-bridge.md` § Disable &
	// revocation: a step-down kills live app sessions and suspends the entire
	// ATProto presence); revoking the sessions alone left the app free to log
	// straight back in on the next request.
	if acct.ActorID == nil || acct.LoginDID == nil || *acct.LoginDID == "" ||
		!acct.ExternalAppsEnabled || len(acct.Verifiers) == 0 {
		auth.VerifyArgon2PHC(req.Password, dummyVerifier)
		fail()
		return
	}
	var matched *wsrpc.AppCredentialVerifier
	for i := range acct.Verifiers {
		if auth.VerifyArgon2PHC(req.Password, acct.Verifiers[i].Verifier) {
			matched = &acct.Verifiers[i]
			break
		}
	}
	if matched == nil {
		fail()
		return
	}

	did := *acct.LoginDID
	scope := ScopeAppPass
	if matched.DmAllowed {
		scope = ScopeAppPassPrivileged
	}
	tokens, err := s.minter.MintSession(did, acct.ActorID, identifier, scope)
	if err != nil {
		s.logger.Warn("createSession: mint failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	if err := wsrpc.RecordSession(ctx, s.nest, acct.ActorID, tokens.SessionID,
		PlaneAppCredential, matched.CredentialID, userAgentNote(r), tokens.ExpiresAt); err != nil {
		// Covers the nest-side kill-switch race too (the disabled RpcError):
		// no session is minted that the registry does not hold.
		s.logger.Warn("createSession: record_session failed", "err", err)
		xrpc.WriteError(w, xrpc.AuthRequired())
		return
	}
	s.flags.set(acct.ActorID, acct.ExternalAppsEnabled)
	s.lock.ResetFor(identifier, "", ip)
	xrpc.WriteJSON(w, map[string]any{
		"accessJwt":  tokens.AccessJwt,
		"refreshJwt": tokens.RefreshJwt,
		"handle":     identifier,
		"did":        did,
		"active":     true,
	})
}

// userAgentNote derives the user-visible client note for the sessions list
// from the login box's User-Agent (trimmed; empty stays empty).
func userAgentNote(r *http.Request) string {
	ua := strings.TrimSpace(r.Header.Get("User-Agent"))
	if len(ua) > 120 {
		ua = ua[:120]
	}
	return ua
}

func (s *Server) refreshSession(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	claims, ok := s.bearerRefreshClaims(r)
	if !ok {
		xrpc.WriteError(w, xrpc.AuthRequired())
		return
	}
	actor, actorErr := claims.ActorBytes()
	sid, sidErr := claims.SidBytes()
	jti, jtiErr := claims.JtiBytes()
	if actorErr != nil || sidErr != nil || jtiErr != nil {
		xrpc.WriteError(w, xrpc.AuthRequired())
		return
	}
	// The rotation carries the ORIGINAL session's DID and actor forward: a
	// refresh re-mints the same session family, so re-resolving either would
	// let a session silently change which account it speaks for.
	rotated, err := s.minter.MintRotation(claims.Sub, actor, claims.Handle, claims.AccessScope(), sid)
	if err != nil {
		s.logger.Warn("refreshSession: mint failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	ctx, cancel := timeoutCtx(r, rpcTimeout)
	defer cancel()
	status, err := wsrpc.RefreshSession(ctx, s.nest, actor, sid, jti, rotated.NewJti, rotated.ExpiresAt)
	if err != nil {
		s.logger.Warn("refreshSession: nest refresh failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	switch status {
	case wsrpc.RefreshStatusRotated:
		xrpc.WriteJSON(w, map[string]any{
			"accessJwt":  rotated.AccessJwt,
			"refreshJwt": rotated.RefreshJwt,
			"handle":     claims.Handle,
			"did":        claims.Sub,
			"active":     true,
		})
	case wsrpc.RefreshStatusReuseDetected:
		// Replay: the family is dead. Uniform refusal (the attacker holding
		// the stale token learns nothing; the legitimate client re-logins).
		s.logger.Warn("refreshSession: refresh-token reuse detected; session family revoked",
			"did", claims.Sub)
		xrpc.WriteError(w, xrpc.AuthRequired())
	default: // invalid / disabled
		xrpc.WriteError(w, xrpc.AuthRequired())
	}
}

func (s *Server) deleteSession(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	claims, ok := s.bearerRefreshClaims(r)
	if !ok {
		xrpc.WriteError(w, xrpc.AuthRequired())
		return
	}
	actor, actorErr := claims.ActorBytes()
	sid, sidErr := claims.SidBytes()
	if actorErr != nil || sidErr != nil {
		xrpc.WriteError(w, xrpc.AuthRequired())
		return
	}
	ctx, cancel := timeoutCtx(r, rpcTimeout)
	defer cancel()
	if _, err := wsrpc.EndSession(ctx, s.nest, actor, sid); err != nil {
		s.logger.Warn("deleteSession: nest end_session failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	w.WriteHeader(http.StatusOK)
}

func (s *Server) getSession(w http.ResponseWriter, _ *http.Request, caller *xrpc.Caller) {
	// The authed no-op (F1's definition-of-success probe): echo the
	// caller's identity from the verified token.
	xrpc.WriteJSON(w, map[string]any{
		"handle": caller.Handle,
		"did":    caller.DID,
		"active": true,
	})
}

// maxPreferencesBytes mirrors fauna_protocol::atproto_pds::PREFERENCES_MAX_BYTES.
// The nest is the source-of-truth authority on the cap; this bridge-side
// pre-check returns a clean XRPC InvalidRequest instead of shipping an
// oversized body to the nest. Keep the two constants in sync.
const maxPreferencesBytes = 100 * 1024

// preferencesBody is the app.bsky.actor.{get,put}Preferences wire shape. The
// bridge treats the `preferences` array as OPAQUE — it persists the exact
// bytes the client sent and returns them verbatim, inventing no Fauna concept
// (D2). Storage lives in nest state (D4 custody), not the bridge.
type preferencesBody struct {
	Preferences json.RawMessage `json:"preferences"`
}

func (s *Server) getPreferences(w http.ResponseWriter, r *http.Request, caller *xrpc.Caller) {
	ctx, cancel := timeoutCtx(r, rpcTimeout)
	defer cancel()
	prefs, err := wsrpc.FetchPreferences(ctx, s.nest, caller.ActorID)
	if err != nil {
		s.logger.Warn("getPreferences: nest fetch failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	// Never stored → the ecosystem default is an empty array, not null.
	body := json.RawMessage(prefs)
	if len(body) == 0 {
		body = json.RawMessage("[]")
	}
	xrpc.WriteJSON(w, preferencesBody{Preferences: body})
}

func (s *Server) putPreferences(w http.ResponseWriter, r *http.Request, caller *xrpc.Caller) {
	var body preferencesBody
	// +4096 slack for the `{"preferences":}` envelope and whitespace; the
	// payload itself is capped below against maxPreferencesBytes exactly.
	dec := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxPreferencesBytes+4096))
	if err := dec.Decode(&body); err != nil {
		xrpc.WriteError(w, xrpc.InvalidRequest("preferences body too large or malformed"))
		return
	}
	// Require a JSON array (the lexicon shape) so a client cannot smuggle a
	// scalar or object into opaque storage. The trimmed array bytes are what we
	// persist and hand straight back on getPreferences.
	payload := bytes.TrimSpace(body.Preferences)
	if len(payload) == 0 || payload[0] != '[' {
		xrpc.WriteError(w, xrpc.InvalidRequest("expected a preferences array"))
		return
	}
	if len(payload) > maxPreferencesBytes {
		xrpc.WriteError(w, xrpc.InvalidRequest("preferences exceed the size limit"))
		return
	}
	ctx, cancel := timeoutCtx(r, rpcTimeout)
	defer cancel()
	if err := wsrpc.StorePreferences(ctx, s.nest, caller.ActorID, payload); err != nil {
		s.logger.Warn("putPreferences: nest store failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	w.WriteHeader(http.StatusOK)
}

func (s *Server) bearerRefreshClaims(r *http.Request) (*Claims, bool) {
	token, ok := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer ")
	if !ok || token == "" {
		return nil, false
	}
	claims, err := s.minter.VerifyRefreshScope(token)
	if err != nil {
		return nil, false
	}
	return claims, true
}

func timeoutCtx(r *http.Request, d time.Duration) (context.Context, context.CancelFunc) {
	return context.WithTimeout(r.Context(), d)
}
