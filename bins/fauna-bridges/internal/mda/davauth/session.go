package davauth

import (
	"context"
	"log/slog"
	"sync"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// Session is the per-request authenticated context carried on
// http.Request.Context() after the auth middleware AEAD-unwraps the
// Basic credential. CalDAV is connection-less (every request
// re-authenticates) so a Session lives for the duration of a single
// HTTP request rather than for an IMAP-style multi-command
// connection.
//
// Per `caldav-server.md` § Process topology: persistent state lives
// nest-side; Session is the ephemeral handle the backend uses to
// route per-request decrypt + RPCs to the right actor. Close
// zeroizes the mlocked MLS capability before the request returns.
type Session struct {
	mu sync.Mutex

	// client is the shared MDA-process WS-RPC caller (one per bridge
	// process). Backend.NewSession sets it; per-request handlers read
	// it via the request context.
	client wsrpc.Caller
	logger *slog.Logger

	// actorID is the AUTH'd 32-byte actor identifier nest returned
	// from validate_recipient.
	actorID []byte
	// credentialID is the credential the AEAD-unwrap succeeded
	// against (today always "default"; future provisioning surfaces
	// will sniff a per-credential id from the username).
	credentialID string
	// authedLocalPart is the email local-part the MUA passed in the
	// Basic Auth username field (e.g. "alice" from
	// "alice@example.com"). Used for the `/caldav/{user@domain}/`
	// path prefix per caldav-server.md § Calendar collection model.
	authedLocalPart string
	// authedDomain is the email domain from the Basic Auth username.
	authedDomain string

	// mlsUnwrap owns the AUTH'd actor's MLS decryption capability.
	// Set on AEAD-unwrap success; Zeroized in (s *Session).Close on
	// request end (via the auth middleware's deferred call). The
	// UniFFI finalizer also zeroizes on GC; the explicit Close path
	// reclaims mlock'd memory eagerly.
	mlsUnwrap *mailfauna.MLSCapability

	// actorMLSPubkey caches the AUTH'd actor's 32-byte X25519 MLS
	// pubkey, fetched right after the AEAD-unwrap. The CalDAV write
	// path (E.3) seals event-body ciphertext to this key before
	// shipping to nest.
	actorMLSPubkey []byte
	// actorMlkemEk caches the AUTH'd actor's 1184-byte ML-KEM-768
	// encapsulation key (the post-quantum half of the same seal key),
	// fetched alongside actorMLSPubkey and set whenever it is: the fetch
	// refuses a key without both halves. The CalDAV and CardDAV body
	// seals seal X-Wing to the pair
	// (`architecture/security/post-quantum.md` § Capability negotiation).
	actorMlkemEk []byte
	// actorIndexKey caches the AUTH'd actor's 32-byte X25519 index
	// pubkey. Nil when nest returns None (Phase E will land
	// production provisioning of index keys end-to-end). Write paths
	// surface the missing-key error at call time; reads succeed
	// regardless.
	actorIndexKey []byte
	// recordOpener is the per-session F2 opener (Phase-3 S2): built ONCE
	// at AUTH from the MLS-snapshot plaintext — fetched encrypted via
	// `fauna.bridges.fetch_mls_snapshot_blob`, AEAD-unwrapped under MSEK
	// by `MlsCapability.Decrypt`, parsed, then the transient plaintext is
	// zeroized (the opener is the ONLY session-lifetime holder of the
	// leaf X25519 init keypairs; a second raw-bytes copy was dropped
	// 2026-07-13). Each per-record open pays one envelope decode + HPKE
	// open instead of the per-open snapshot marshal + re-parse
	// MlsCapability.OpenMailRecord paid. Nil when nest has no snapshot on
	// file yet — AUTH succeeds in that case so the MUA can complete the
	// well-known-URL discovery / PROPFIND-against-principal probes; the
	// per-record open surfaces the missing-snapshot error at call time.
	// Zeroized on Close alongside the capability.
	recordOpener *mailfauna.MailRecordOpener
}

// NewSession builds a Session for an already-AUTH'd principal, wired to the
// given WS-RPC caller. The auth middleware constructs sessions internally
// (resolve, after a successful AEAD-unwrap, additionally populating the actor's
// MLS capability + public keys + snapshot); NewSession is the exported seam a
// consuming terminator's tests (caldav, carddav) use to drive their handler
// code paths against a synthetic session without standing up the full
// Basic-auth round-trip. The MLS capability, public keys, and snapshot bytes
// are left unset — tests exercising the seal/open paths go through the auth
// flow instead. Passing a nil logger falls back to slog.Default.
func NewSession(client wsrpc.Caller, logger *slog.Logger, actorID []byte, localPart, domain string) *Session {
	if logger == nil {
		logger = slog.Default()
	}
	return &Session{
		client:          client,
		logger:          logger,
		actorID:         actorID,
		authedLocalPart: localPart,
		authedDomain:    domain,
	}
}

// Close zeroizes the per-request MLS capability. Safe to call
// multiple times; idempotent.
func (s *Session) Close() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.mlsUnwrap != nil {
		s.mlsUnwrap.Zeroize()
		s.mlsUnwrap = nil
	}
	if s.recordOpener != nil {
		s.recordOpener.Zeroize()
		s.recordOpener = nil
	}
	s.actorMLSPubkey = nil
	s.actorMlkemEk = nil
	s.actorIndexKey = nil
	s.actorID = nil
	s.credentialID = ""
	s.authedLocalPart = ""
	s.authedDomain = ""
}

// RecordOpener returns the per-session F2 record opener built at AUTH
// from the MLS-snapshot plaintext (parsed once; each Open pays only the
// envelope decode + HPKE open). Nil when nest had no snapshot on file
// at AUTH time — callers surface the missing-snapshot error at open
// time. Callers must not Zeroize it directly — Close on the session
// owns the lifecycle (mirrors MLSUnwrap).
func (s *Session) RecordOpener() *mailfauna.MailRecordOpener {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.recordOpener
}

// ActorID returns the AUTH'd 32-byte actor identifier. Read-only;
// returns a copy so callers cannot mutate session state.
func (s *Session) ActorID() []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.actorID == nil {
		return nil
	}
	out := make([]byte, len(s.actorID))
	copy(out, s.actorID)
	return out
}

// AuthedLocalPart returns the localpart of the Basic-Auth username.
func (s *Session) AuthedLocalPart() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.authedLocalPart
}

// AuthedDomain returns the domain of the Basic-Auth username.
func (s *Session) AuthedDomain() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.authedDomain
}

// Client returns the WS-RPC caller wired into this session. Used by
// the backend to dispatch list_calendars, provision_calendar, etc.
// against nest under the AUTH'd actor.
func (s *Session) Client() wsrpc.Caller {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.client
}

// MLSUnwrap returns the AUTH'd actor's MLS capability. Callers must
// not Zeroize it directly — Close on the session owns the lifecycle.
func (s *Session) MLSUnwrap() *mailfauna.MLSCapability {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.mlsUnwrap
}

// MLSPubkey returns the AUTH'd actor's 32-byte X25519 MLS pubkey.
func (s *Session) MLSPubkey() []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.actorMLSPubkey == nil {
		return nil
	}
	out := make([]byte, len(s.actorMLSPubkey))
	copy(out, s.actorMLSPubkey)
	return out
}

// MlkemEk returns the AUTH'd actor's 1184-byte ML-KEM-768 encapsulation
// key (the post-quantum half of the seal key MLSPubkey returns), or nil on
// a session with no key cached. The CalDAV and CardDAV body seals (put.go,
// encrypted_metadata.go) pass it to mailfauna.EncryptToRecipientHybrid,
// which seals X-Wing to the pair. Returns a copy so callers cannot mutate
// session state.
func (s *Session) MlkemEk() []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.actorMlkemEk == nil {
		return nil
	}
	out := make([]byte, len(s.actorMlkemEk))
	copy(out, s.actorMlkemEk)
	return out
}

// IndexKey returns the AUTH'd actor's 32-byte X25519 index pubkey, or
// nil when nest had no index key on file at AUTH time. CalDAV PUT
// falls back to MLSPubkey when this is nil — mirrors the IMAP APPEND
// path so the recipient's MDA opens both shapes identically (see
// `bins/fauna-bridges/internal/mda/imap/append.go` Phase-E gap
// fallback). Returns a copy so callers cannot mutate session state.
func (s *Session) IndexKey() []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.actorIndexKey == nil {
		return nil
	}
	out := make([]byte, len(s.actorIndexKey))
	copy(out, s.actorIndexKey)
	return out
}

// ── request-context plumbing ─────────────────────────────────────

// sessionCtxKey is the unexported context key the auth middleware
// uses to stash the Session for downstream handlers. Backend
// methods recover the Session via `SessionFromContext`.
type sessionCtxKey struct{}

// withSession returns a copy of `ctx` carrying `sess`.
func withSession(ctx context.Context, sess *Session) context.Context {
	return context.WithValue(ctx, sessionCtxKey{}, sess)
}

// ContextWithSession returns a copy of `ctx` carrying `sess` — the seam a
// terminator's second auth arm (WebDAV's bearer door, which admits a
// principal through the nest rather than a Basic AEAD-unwrap) uses to hand the
// same per-request Session to the handlers. The arm owns the session's Close.
func ContextWithSession(ctx context.Context, sess *Session) context.Context {
	return withSession(ctx, sess)
}

// SessionFromContext returns the Session the auth middleware
// attached to `ctx`. Returns nil when the request was never
// authenticated (the middleware would have already short-circuited
// with 401; this nil-check is defensive against future routing
// changes).
func SessionFromContext(ctx context.Context) *Session {
	v, _ := ctx.Value(sessionCtxKey{}).(*Session)
	return v
}
