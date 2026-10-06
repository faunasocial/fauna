// Package davauth is the shared DAV-over-HTTP authentication machinery for the
// mail bridge's DAV terminators (CalDAV today; CardDAV and a future WebDAV-files
// protocol tomorrow). It holds the HTTP-Basic → AEAD-unwrap middleware, the
// per-session auth-material cache, and the per-request Session that carries the
// AUTH'd actor's MLS material to the protocol handlers — the genuinely-shared
// glue each terminator would otherwise duplicate over the same
// internal/{auth,authlock,mailfauna,wsrpc} packages. The only per-terminator
// knob is the WWW-Authenticate realm, threaded through NewMiddleware
// ("fauna-caldav", "fauna-carddav", …).
package davauth

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"sync/atomic"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/auth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// ErrAuthFailed is the opaque error every auth-failure path returns
// to the wire. Mirrors imap/auth.go's errAuthFailed — a probe
// attacker can't tell "wrong password" from "wrong user" from
// "blob fetch transport failure". Exported so a consuming terminator's
// interceptors can return the identical opaque body on a defensive
// no-session 401 (e.g. caldav's props/sync-collection/mkcalendar chain).
var ErrAuthFailed = errors.New("davauth: authentication failed")

// The credential_id is resolved per request from the Basic-Auth username's RFC
// 5233 "+suffix" (auth.CredentialFromLocalPart) — `<handle>+<credential_id>@…`,
// defaulting to auth.DefaultCredentialID for a bare username. See
// `docs/goal/behavior/mail-credentials.md` § MUA-username convention.

// authMiddleware wraps `next` with HTTP Basic Auth → AEAD-unwrap.
// On success, the request context carries a `*Session` (recoverable
// via `SessionFromContext`); on failure the request is short-
// circuited with `401 Unauthorized`.
//
// Per `caldav-server.md` § Authentication: HTTPS-only (the listener
// already does TLS termination — this handler doesn't re-check) +
// the same AEAD-unwrap-as-auth flow as IMAP. AEAD-success is the
// AUTH signal; nest never sees the credential bytes.
//
// `now` is overridable in tests to assert audit timestamps without
// sleeping; nil falls back to `time.Now`.
type authMiddleware struct {
	// realm is the WWW-Authenticate realm advertised on 401 responses
	// (RFC 7617 requires one). Per-instance, not a package const, so each
	// DAV terminator scopes its Basic-auth challenge to its own service
	// ("fauna-caldav", "fauna-carddav", …) off the same middleware.
	realm  string
	next   http.Handler
	client wsrpc.Caller
	logger *slog.Logger
	now    func() time.Time
	// lockout is the per-(credential, source-IP) AUTH-failure brake,
	// shared with the owning Server (which hot-swaps the pointed-to
	// instance on a `config_changed` re-fetch via Server.ApplyConfig).
	// Queried before validate_recipient + the Argon2id unwrap so a locked
	// key pays neither the nest round-trip nor the KDF (security review
	// § D4/M1). The atomic pointer is never nil (NewMiddleware
	// defaults a disabled lockout); a Load() before any Store yields nil,
	// which authlock treats as "never locked".
	lockout *atomic.Pointer[authlock.Lockout]
	// cache holds the per-(actor, credential) nest-fetched auth material
	// for authMaterialTTL so a burst of stateless CalDAV requests doesn't
	// re-fetch — and re-rate-limit — the wrapped MLS blob on nest for
	// every PROPFIND/REPORT/PUT. The per-request AEAD-unwrap of the
	// presented password (resolve()) still runs on a cache hit, so this
	// does not weaken AEAD-as-AUTH. See authMaterialCache.
	cache *authMaterialCache
	// primaryDomain is the box's PrimaryDomain (ConfigSnapshot.PrimaryDomain),
	// hot-swapped by Server.ApplyConfig on a `config_changed` re-fetch. A
	// Basic-auth username with no domain — what macOS Calendar.app sends after
	// stripping the domain from a configured email — resolves under it
	// (auth.SplitEmailDefault). Nil/empty ⇒ strict `user@domain` only.
	primaryDomain *atomic.Pointer[string]
}

// currentPrimaryDomain reads the hot-swappable primary domain, defaulting to
// "" (strict `user@domain`) when unset.
func (m *authMiddleware) currentPrimaryDomain() string {
	if m.primaryDomain == nil {
		return ""
	}
	if p := m.primaryDomain.Load(); p != nil {
		return *p
	}
	return ""
}

// NewMiddleware wraps `next` with Basic-Auth → AEAD-unwrap. `realm` is the
// per-terminator WWW-Authenticate realm advertised on 401s ("fauna-caldav",
// "fauna-carddav", …). `lockout` is the shared AUTH-failure brake; nil installs
// a disabled one (tests that don't exercise the lockout).
func NewMiddleware(realm string, next http.Handler, client wsrpc.Caller, logger *slog.Logger, lockout *atomic.Pointer[authlock.Lockout], primaryDomain *atomic.Pointer[string]) http.Handler {
	if next == nil {
		panic("davauth: NewMiddleware: next must not be nil")
	}
	if client == nil {
		panic("davauth: NewMiddleware: client must not be nil")
	}
	if logger == nil {
		logger = slog.Default()
	}
	if lockout == nil {
		lockout = &atomic.Pointer[authlock.Lockout]{}
		lockout.Store(authlock.New(0, time.Minute, nil)) // disabled
	}
	return &authMiddleware{
		realm:         realm,
		next:          next,
		client:        client,
		logger:        logger,
		now:           time.Now,
		lockout:       lockout,
		cache:         newAuthMaterialCache(),
		primaryDomain: primaryDomain,
	}
}

// clientIP returns the host part of the request's TCP RemoteAddr. CalDAV
// (:443) is fronted by the SNI router; the router now prepends a PROXY-v2
// header (--send-proxy-to 127.0.0.1:8444) that the listener peels in
// internal/proxyproto, so RemoteAddr — and therefore this — is the real client
// IP, giving the lockout key its full cross-account per-IP dimension and a
// meaningful per-IP report_auth_event audit. A direct headerless dial keeps its
// genuine peer, so this stays non-empty (report_auth_event rejects an empty
// source_ip) in every case.
func clientIP(r *http.Request) string {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
}

func (m *authMiddleware) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	sourceIP := clientIP(r)
	username, password, ok := r.BasicAuth()
	if !ok {
		m.replyUnauthorized(w, nil, "missing basic auth", sourceIP)
		return
	}
	local, domain, ok := auth.SplitEmailDefault(username, m.currentPrimaryDomain())
	if !ok {
		m.replyUnauthorized(w, nil, "malformed username", sourceIP)
		return
	}
	// RFC 5233 sub-addressing: `<base>+<credential_id>@<domain>`. The base
	// resolves the actor; the suffix selects the wrapped-MSEK blob. A bare
	// username maps to DefaultCredentialID (mail-credentials.md § MUA-username).
	base, credentialID := auth.CredentialFromLocalPart(local)
	// The lockout keys on the canonical (base, domain) principal — NOT the raw
	// presented username — so every form that resolves to this credential
	// (`alice`, `alice+`, `alice+default`, `alice@<primary>`) shares one bucket
	// (auth.PrincipalKey; § B7). The raw username is still logged for forensics.
	principal := auth.PrincipalKey(base, domain)

	// Per-(credential, source-IP) lockout: refuse before validate_recipient
	// and the Argon2id unwrap once the key is locked (security review
	// § D4/M1), so a brute-forcer pays neither the nest round-trip nor the
	// KDF cost. The locked branch skips report_auth_event — like the MTA /
	// IMAP, it avoids inflating the audit log during a sustained attack and
	// surfaces a bridge-local warn instead.
	lockout := m.lockout.Load()
	if lockout.IsLockedFor(principal, credentialID, sourceIP) {
		m.logger.Warn("davauth: AUTH locked out", "username", username, "source_ip", sourceIP)
		w.Header().Set("WWW-Authenticate", fmt.Sprintf(`Basic realm=%q`, m.realm))
		http.Error(w, ErrAuthFailed.Error(), http.StatusUnauthorized)
		return
	}

	ctx, cancel := context.WithTimeout(r.Context(), auth.RPCTimeout)
	defer cancel()

	sess, err := m.resolve(ctx, base, domain, credentialID, password)
	if err != nil {
		lockout.RecordFailureFor(principal, credentialID, sourceIP)
		m.replyUnauthorized(w, sess, err.Error(), sourceIP)
		return
	}
	lockout.ResetFor(principal, credentialID, sourceIP)
	// Successful unwrap — emit the success audit event so nest's
	// audit log carries the AUTH-OK record before the request runs.
	// A nest-side failure here is non-fatal for AUTH (the user
	// already unwrapped successfully); we log + continue.
	occurredAt := uint64(m.now().UnixMilli())
	if err := wsrpc.ReportAuthEvent(
		ctx, m.client, sess.actorID, sess.credentialID, "ok", sourceIP, "", occurredAt,
	); err != nil {
		m.logger.Warn("davauth: report_auth_event(ok) failed", "err", err)
	}

	// Hand the request to the next handler with the Session
	// stashed in context. Defer Close so the mlocked capability is
	// zeroized regardless of how the downstream handler returns.
	defer sess.Close()
	r = r.WithContext(withSession(r.Context(), sess))
	m.next.ServeHTTP(w, r)
}

// resolve runs the AEAD-unwrap-as-auth flow for one Basic-auth attempt:
//
//  1. Obtain the actor's auth material — actor_id + wrapped MLS blob +
//     MLS/index pubkeys + encrypted MLS-snapshot blob — from the
//     per-(actor, credential) cache, or fetch it from nest on a miss
//     (fetchAuthMaterial). The material is password-independent, so a
//     warm cache collapses a stateless-CalDAV request burst to a single
//     nest fetch and keeps it under nest's `bridge_rate_limit`.
//  2. UnwrapMLSBlob(blob, password, …) → mlocked MLSCapability.
//     AEAD-success is the AUTH signal. This runs on EVERY request (hit
//     or miss), so a cache hit still verifies the presented password —
//     the cache never stores the unwrapped capability or the password.
//  3. Decrypt the MLS-snapshot blob under the freshly-unwrapped
//     capability (per request, from the cached ciphertext).
//
// On any failure returns errAuthFailed wrapping the reason; the reason
// is logged + audit-reported but never surfaced over the wire. The
// returned `*Session` is always non-nil on success and
// non-nil-but-actor-ID-only on failure (so the caller can audit against
// the resolved actor when applicable).
func (m *authMiddleware) resolve(ctx context.Context, local, domain, credentialID, password string) (*Session, error) {
	sess := &Session{
		client:          m.client,
		logger:          m.logger,
		authedLocalPart: local,
		authedDomain:    domain,
	}

	key := materialKey(local, domain, credentialID)
	material, ok := m.cache.get(key, m.now())
	if !ok {
		var err error
		material, err = m.fetchAuthMaterial(ctx, local, domain, credentialID)
		if err != nil {
			// The user/credential could not be resolved (unknown user). Run a
			// dummy KDF so this path costs the same Argon2id as the real
			// per-request unwrap below; otherwise the KDF-vs-no-KDF timing gap
			// enumerates valid accounts (network-exposure.md § Rulings F3).
			mailfauna.DummyCredentialKDF([]byte(password), mailfauna.KdfKindArgon2id)
			// fetchAuthMaterial returns a (partial) material carrying the
			// resolved actor_id even on a post-validate fetch failure, so
			// the caller can still audit the AUTH-fail against the actor.
			if material != nil && material.actorID != nil {
				sess.actorID = material.actorID
				sess.credentialID = credentialID
			}
			return sess, err
		}
		// Cache only fully-fetched material (never a failure). Caching
		// regardless of the password outcome is correct: the material is
		// the actor's real blob (nest hands it on MDA trust, not on the
		// password), and the next request's unwrap re-verifies the
		// password against it.
		m.cache.put(key, material, m.now())
	}
	sess.actorID = material.actorID
	sess.credentialID = credentialID

	// Per-request AEAD-unwrap = password verification (NEVER cached).
	cap, err := mailfauna.UnwrapMLSBlob(
		material.wrappedBlob, []byte(password), material.actorID, credentialID, mailfauna.KdfKindArgon2id,
	)
	if err != nil {
		return sess, fmt.Errorf("unwrap_mls_blob: %w", err)
	}
	sess.mlsUnwrap = cap
	sess.actorMLSPubkey = material.pubkey
	sess.actorMlkemEk = material.mlkemEk
	sess.actorIndexKey = material.indexKey

	// MLS-snapshot blob: AEAD-unwrap under MSEK so the per-request
	// metadata-unseal path (PROPFIND/REPORT against the lazy-Personal
	// calendar; the per-event open path in E.3) can hand the plaintext to
	// MlsCapability.OpenMailRecord. Nil = no snapshot provisioned yet;
	// AUTH still succeeds so the MUA can complete the well-known-URL
	// dance, the per-collection unseal surfaces the missing-snapshot error
	// at call time. AEAD-unwrap failure = auth-fail (the wrapped MSEK
	// already unwrapped, so a mismatched snapshot blob is a real integrity
	// problem).
	if material.snapshotBlob != nil {
		plaintext, err := cap.Decrypt(material.snapshotBlob)
		if err != nil {
			cap.Zeroize()
			sess.mlsUnwrap = nil
			return sess, fmt.Errorf("decrypt mls-snapshot: %w", err)
		}
		// Per-session F2 record opener (Phase-3 S2): parse the snapshot
		// plaintext ONCE here so every per-record open on this request
		// skips the snapshot marshal + re-parse. A parse failure is the
		// same failure class as the AEAD failure above — the MSEK already
		// unwrapped, so a snapshot that decrypts but does not parse is a
		// real integrity problem; fail the AUTH rather than hand handlers
		// a half-usable session. The opener is the ONLY session-lifetime
		// holder of the leaf secrets — the transient plaintext is
		// zeroized right here, both arms (a session-lifetime raw copy
		// was dropped 2026-07-13).
		opener, err := mailfauna.NewMailRecordOpener(plaintext)
		for i := range plaintext {
			plaintext[i] = 0
		}
		if err != nil {
			cap.Zeroize()
			sess.mlsUnwrap = nil
			return sess, fmt.Errorf("parse mls-snapshot: %w", err)
		}
		sess.recordOpener = opener
	}
	return sess, nil
}

// fetchAuthMaterial performs the nest round-trips that resolve an actor's
// password-independent auth material:
//
//  1. validate_recipient(local, domain) → actor_id.
//  2. fetch_wrapped_mls_blob(actor_id, credential_id) → blob bytes.
//  3. fetch_recipient_mls_pubkey(actor_id) → pubkey + ML-KEM ek, both
//     halves or an error (the fetch refuses a key missing either).
//  4. fetch_recipient_index_key(actor_id) → index key (may be nil).
//  5. fetch_mls_snapshot_blob(actor_id) → snapshot blob (may be nil).
//
// The result is safe to cache across requests (every field is ciphertext
// or a public key — see authMaterial); the per-request AEAD-unwrap that
// actually verifies the password happens in resolve(), never here.
//
// On a post-validate failure the returned material still carries the
// resolved actor_id so the caller can audit the AUTH-fail against the
// actor; on a validate failure it returns (nil, err). The extra
// pubkey/index/snapshot fetches happen here before the password is
// verified (unlike the prior unwrap-first ordering); this leaks nothing
// to a probe (the values stay in MDA memory, surfaced to the session only
// after a successful unwrap) and is bounded by the AUTH-failure lockout +
// the cache (a brute-forcer warms the entry once, then re-uses it).
func (m *authMiddleware) fetchAuthMaterial(ctx context.Context, local, domain, credentialID string) (*authMaterial, error) {
	actorID, _, err := wsrpc.ValidateRecipient(ctx, m.client, local, domain)
	if err != nil {
		return nil, fmt.Errorf("validate_recipient: %w", err)
	}
	if len(actorID) != 32 {
		return nil, fmt.Errorf("validate_recipient actor_id must be 32 bytes, got %d", len(actorID))
	}
	mat := &authMaterial{actorID: actorID}

	blob, err := wsrpc.FetchWrappedMLSBlob(ctx, m.client, actorID, credentialID)
	if err != nil {
		return mat, fmt.Errorf("fetch_wrapped_mls_blob: %w", err)
	}
	if blob == nil {
		return mat, fmt.Errorf("fetch_wrapped_mls_blob: nest has no blob on file")
	}
	mat.wrappedBlob = blob

	// Session-level pubkey caching, NOT mail-new-ingest: this feeds
	// CalDAV/CardDAV PUT + collection-metadata seals that have no epoch
	// opener, so it must always resolve the standing key even with the
	// content-sealing-epochs write gate forced on.
	pubkey, mlkemEk, _, err := wsrpc.FetchRecipientMLSPubkeyHybrid(ctx, m.client, actorID, false)
	if err != nil {
		return mat, fmt.Errorf("fetch_recipient_mls_pubkey: %w", err)
	}
	mat.pubkey = pubkey
	mat.mlkemEk = mlkemEk

	indexKey, err := wsrpc.FetchRecipientIndexKey(ctx, m.client, actorID)
	if err != nil {
		return mat, fmt.Errorf("fetch_recipient_index_key: %w", err)
	}
	mat.indexKey = indexKey

	snapshotBlob, err := wsrpc.FetchMLSSnapshotBlob(ctx, m.client, actorID)
	if err != nil {
		return mat, fmt.Errorf("fetch_mls_snapshot_blob: %w", err)
	}
	mat.snapshotBlob = snapshotBlob

	return mat, nil
}

// replyUnauthorized fires the fail audit event (when an actor was
// resolved) and emits a `401 Unauthorized` with the standard
// WWW-Authenticate challenge. `sourceIP` is stamped on the audit row so
// nest can key per-IP (M1) — the real client IP now that the listener peels
// the router's PROXY-v2 header (internal/proxyproto); always non-empty, since
// nest rejects an empty source.
func (m *authMiddleware) replyUnauthorized(w http.ResponseWriter, sess *Session, reason, sourceIP string) {
	// Audit only when we have an actor (a malformed Basic-Auth
	// header that never reached validate_recipient produces no
	// audit row; the audit log is for credential-against-actor
	// outcomes, not for malformed-header probes).
	if sess != nil && sess.actorID != nil && m.client != nil {
		ctx, cancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
		defer cancel()
		occurredAt := uint64(m.now().UnixMilli())
		if err := wsrpc.ReportAuthEvent(
			ctx, m.client, sess.actorID, sess.credentialID, "fail", sourceIP, auth.ReasonRedacted, occurredAt,
		); err != nil {
			m.logger.Warn("davauth: report_auth_event(fail) failed", "report_err", err, "auth_reason", reason)
		}
		// Zeroize any partially-built capability before the request
		// returns.
		sess.Close()
	}
	m.logger.Info("davauth: AUTH failed", "reason", reason, "source_ip", sourceIP)
	w.Header().Set("WWW-Authenticate", fmt.Sprintf(`Basic realm=%q`, m.realm))
	http.Error(w, ErrAuthFailed.Error(), http.StatusUnauthorized)
}
