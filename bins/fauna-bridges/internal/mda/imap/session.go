package imap

import (
	"context"
	"errors"
	"log/slog"
	"sync"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/auth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// errNotImplemented is the placeholder the one authenticated-state
// command method still stubbed (Status) returns. Per
// imap-server.md, returning a typed error here translates to
// `<tag> NO <message>` on the IMAP wire (emersion/go-imap does the
// status-response mapping for us).
var errNotImplemented = errors.New("imap: not implemented yet (Phase C/D/F scope)")

// Session implements imapserver.SessionIMAP4rev2 for one IMAP TCP
// connection. It carries the state-bearing fields + Close zeroization
// + report_session_close; the per-command methods live in their own
// files, and only Status still returns errNotImplemented.
//
// All mutation is mu-guarded. The mlsUnwrap field (when populated by
// C.3) holds an mlocked AEAD key; Close zeroizes it before returning.
type Session struct {
	mu sync.Mutex

	// Wired by Backend.NewSession on connect:
	conn   *imapserver.Conn
	client wsrpc.Caller
	logger *slog.Logger
	// sourceIP is the connecting client's IP (host part of the TCP
	// RemoteAddr), wired by Backend.NewSession. The IMAP listeners
	// (993/143) are published directly (docker-compose `993:993`), so
	// this is the real client IP — not a loopback proxy address — which
	// is what both the per-(credential, IP) AUTH lockout below and the
	// `report_auth_event` audit row key on. Empty only in unit tests that
	// construct a Session by hand.
	sourceIP string
	// lockout is the per-(credential, source-IP) AUTH-failure brake,
	// shared across every Session the Backend produces and snapshotted
	// here at connect (a `config_changed` rebuild swaps the Backend's
	// pointer; an in-flight session keeps the instance live at its start).
	// The auth path queries IsLocked *before* the validate_recipient
	// round-trip + the Argon2id AEAD-unwrap, so a locked (credential, IP)
	// pays neither the KDF cost nor an AEAD-timing oracle (security review
	// § D4/M1). nil-safe: a hand-built test Session leaves it nil, which
	// authlock treats as "never locked".
	lockout *authlock.Lockout
	// primaryDomain is the box's PrimaryDomain, snapshotted from the Backend
	// at connect. A username with no domain resolves under it
	// (auth.SplitEmailDefault) — uniform with the CalDAV + MTA surfaces so a
	// bare username behaves the same everywhere. Empty ⇒ strict `user@domain`.
	primaryDomain string
	// nowFn is overridable in tests to assert occurred_at timestamps
	// in report_session_close calls without sleeping.
	nowFn func() time.Time

	// Set by C.3 AUTH:
	actorID      []byte
	credentialID string
	// authedLocalPart is the email localPart the MUA passed during
	// AUTH (e.g. "alice" from "alice@example.com"). Used as the
	// QUOTA root display name in `Session.GetQuotaRoot` — the
	// canonical `users.handle` resolution is a Phase F+ track when
	// emersion lands QUOTA wire serving (see imap-server.md
	// § Upstream-blocked gaps).
	authedLocalPart string
	// mlsUnwrap holds the AUTH'd actor's MLS decryption capability.
	// Populated on AEAD-unwrap success; zeroized on Close. The UniFFI
	// finalizer also zeroizes on GC; the explicit Zeroize call in
	// Close reclaims the mlock'd memory eagerly and makes the intent
	// readable in code review.
	mlsUnwrap *mailfauna.MLSCapability
	// actorMLSPubkey caches the AUTH'd actor's 32-byte X25519 MLS
	// pubkey (fetched from nest right after the AEAD-unwrap). Phase
	// C.6 consumes this when verifying group state signatures off
	// the unwrapped snapshot; today the AUTH path just caches it so
	// callers don't need to re-fetch.
	actorMLSPubkey []byte
	// actorMlkemEk caches the AUTH'd actor's 1184-byte ML-KEM-768
	// encapsulation key (the post-quantum half of the same seal key),
	// fetched alongside actorMLSPubkey and set whenever it is: the fetch
	// refuses a key without both halves. APPEND's seal-to-self body seal
	// seals X-Wing to the pair
	// (`architecture/security/post-quantum.md` § Capability negotiation).
	actorMlkemEk []byte
	// actorIndexKey caches the AUTH'd actor's 32-byte X25519 index
	// pubkey (fetched from nest right after the MLS pubkey). I5
	// Phase D.5 APPEND seals the search-index hint to this key.
	// Nil when nest returns None (Phase E will land production
	// provisioning); APPEND surfaces the missing-index-key error
	// at call time, AUTH succeeds regardless so the user can still
	// read mail without seal-to-self writes.
	actorIndexKey []byte
	// recordOpener is the per-connection mail-record opener (Phase-3 S2 /
	// the F2 perf fold): the AUTH'd actor's MLS-snapshot plaintext —
	// fetched encrypted via `fauna.bridges.fetch_mls_snapshot_blob`, then
	// AEAD-unwrapped under MSEK by the existing `MlsCapability.Decrypt` —
	// is parsed ONCE at AUTH into this opener (the leaf X25519 init
	// keypairs live Rust-side), and every per-record serve-path open
	// (`mailfauna.OpenStoredRecord`) pays only the envelope decode + HPKE
	// open. Nil when nest has no snapshot on file yet — AUTH succeeds in
	// that case so the MUA can list mailboxes / inspect flags; a
	// sealed-record FETCH BODY surfaces the missing-snapshot error at
	// open time. Zeroized on Close alongside mlsUnwrap.
	recordOpener *mailfauna.MailRecordOpener
	// indexSession is this MUA session's handle on the actor's sealed
	// mail/calendar content-index slice (content-index.md § Where the index is
	// built — the MDA builder leg). Minted at AUTH from the same MlsCapability
	// as recordOpener, so the MSEK never crosses the FFI boundary; it drives the
	// SAME shared Rust builder the client leg uses, over the Go transport in
	// index_rail.go.
	//
	// **Nil is a normal, non-fatal state and the whole fallback story.** No
	// snapshot yet, or a rail read that failed —
	// any of these leave it nil and `SEARCH` answers its body
	// axis the pre-backend-2 way (a linear scan over per-message hints), exactly
	// as the client leg skips a builder launch rather than failing login. Nothing
	// is lost: the next session re-presents the same mailbox.
	//
	// Zeroized on Close alongside mlsUnwrap and recordOpener.
	indexSession *faunaFfi.FfiMailIndexSession

	// Set by C.5 SELECT/EXAMINE:
	selectedMailbox     string
	selectedUIDValidity uint32
	lastKnownModseq     int64
	condStoreEnabled    bool
	qresyncEnabled      bool

	// Shared by the Backend across all sessions. Reads/writes hold the
	// cache's own mutex internally; never under `s.mu`.
	cache *bodyStructureCache

	// plane is the Backend's bulk-byte-plane client, snapshotted at
	// NewSession: a body too large for the 2 MiB WS-RPC frame arrives as a
	// reference, and every body read on this session resolves it through
	// SealedBodyOf (body_ref.go). nil is legal — see backend.plane.
	plane *byteplane.Client

	// Phase F IDLE/NOTIFY: backend-owned notification router demuxes
	// pushed `BridgeMailboxStatePush` frames by subscription_id to the
	// per-IDLE-call channel each session.Idle registers. The Backend
	// installs router.Handle as the wsrpc.Client.OnPush callback at
	// construction time so all Sessions share one push consumer
	// goroutine on the wsrpc reader.
	router *notificationRouter
	// idleTimeout is the per-server IDLE timeout (RFC 2177 §3; default
	// 29 minutes per imap-server.md § IDLE; configurable via
	// `imap.idle_timeout_seconds` mail-policy-config knob). Plumbed
	// through Backend from `Snapshot.IMAP.IdleTimeoutSecs`.
	idleTimeout time.Duration
	// idleFetcher is the test seam Session.idle uses to translate UIDs
	// to sequence numbers per event. nil in production (the inner
	// idle() falls back to a wsrpcMetadataFetcher wrapping s.client);
	// tests inject a canned-snapshot fetcher.
	idleFetcher metadataFetcher

	// spamPolicy is the snapshot-derived per-user-scorer config the
	// SELECT-time scoring pass (spam_score.go) routes INBOX→Junk
	// against. Snapshotted from the Backend at NewSession so a session
	// opened after an admin `put_spam_policy` edit picks up the new
	// `spam_folder` threshold at connect (in-flight sessions keep their
	// copy — same request-boundary hot-reload as idleTimeout).
	spamPolicy mailfauna.SpamPolicy
	// bayesianKnobs is the snapshot-derived confidence-ramp / weight config the
	// SELECT-time scorer feeds to the shared per-user Bayesian scorer. Snapshotted
	// from the Backend at NewSession alongside spamPolicy (same request-boundary
	// hot-reload). The zero value is harmless for tests that inject scoreFn (the
	// FFI path is bypassed); the production path reads the admin-effective knobs.
	bayesianKnobs mailfauna.BayesianKnobs
	// scoreFn is the test seam scoreSelectedInbox uses for the
	// per-message Bayesian scorer. nil in production (it falls back to
	// the shared cgo FFI scorer with the catalog knobs); tests inject a
	// deterministic scorer so the pass's watermark + re-file logic is
	// exercised without the FFI.
	scoreFn func(modelBytes []byte, text string) int32
	// scoreOpener is the announce-time-scoring test seam. The IDLE append
	// gate (scored-before-visible, idle.go) resolves the record opener via
	// announceScoreOpener, which returns this when set. nil in production —
	// the per-connection recordOpener is used, which needs a real MLS
	// snapshot the unit tests can't build; a stub opener is injected here so
	// the append gate's score-before-announce ordering is exercised.
	scoreOpener mailfauna.RecordOpener
}

// announceScoreOpener resolves the record opener for an announce-time
// scoring pass (the IDLE append gate — scored-before-visible). Returns the
// scoreOpener test seam when set, else the per-connection recordOpener as a
// RecordOpener interface (the explicit nil-interface dance so a nil concrete
// opener stays a nil interface — mirroring Select/Fetch). Read under s.mu.
func (s *Session) announceScoreOpener() mailfauna.RecordOpener {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.scoreOpener != nil {
		return s.scoreOpener
	}
	if s.recordOpener != nil {
		return s.recordOpener
	}
	return nil
}

// Close zeroizes per-session secrets and reports the session close to
// nest. Always returns nil — close-time errors are logged, never
// surfaced to the caller (the IMAP TCP connection is going away
// regardless; we can't usefully recover from a failed RPC at teardown).
//
// Per imap-server.md § Authentication: on LOGOUT / idle timeout /
// session disconnect, the MDA zeroizes any unwrapped capability and
// calls report_session_close(actor_id, credential_id, reason).
func (s *Session) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	// Zeroize first — even if the report RPC fails, the secret is
	// gone from this process's address space.
	if s.mlsUnwrap != nil {
		s.mlsUnwrap.Zeroize()
	}
	s.mlsUnwrap = nil
	s.actorMLSPubkey = nil
	s.actorMlkemEk = nil
	s.actorIndexKey = nil
	// The record opener holds the snapshot's leaf X25519 secrets
	// (HPKE init keys) Rust-side; zeroize them eagerly with the cap.
	if s.recordOpener != nil {
		s.recordOpener.Zeroize()
	}
	s.recordOpener = nil
	// The index session holds the MSEK-derived index-segment key ring (and the
	// grace generations from the snapshot) Rust-side; drop them with the rest.
	// Anything staged but unflushed is discarded on purpose — the docs are
	// re-derivable from the mail, and publishing during teardown would put a
	// rail round-trip on the LOGOUT path.
	if s.indexSession != nil {
		s.indexSession.Zeroize()
	}
	s.indexSession = nil
	// Report only when AUTH succeeded (actorID is non-nil).
	if s.actorID != nil && s.client != nil {
		// Use a short deadline so a stuck nest doesn't block conn
		// teardown. The fire-and-forget pattern is OK here — the
		// audit log can drop a teardown-time event; the next AUTH
		// will replace the corresponding open-session record anyway.
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		occurredAt := s.now().UnixMilli()
		if err := wsrpc.ReportSessionClose(ctx, s.client, s.actorID, s.credentialID, "logout", occurredAt); err != nil {
			if s.logger != nil {
				s.logger.Warn("imap: report_session_close failed", "err", err)
			}
		}
	}
	s.actorID = nil
	s.credentialID = ""
	s.authedLocalPart = ""
	return nil
}

// IdleTimeout reports this session's per-server IDLE timeout (RFC 2177 §3).
// The vendored imapserver's handleIdle reads it (via an optional-method type
// assertion) to set the IDLE *connection read deadline*, so the MDA actually
// ends an inactive IDLE with `* BYE` when the timeout fires — the seam-level
// timer in Session.idle returns but cannot close the wire connection itself.
// Baked at NewSession from the Backend's current (atomic) value, so a new
// connection opened after a `config_changed` hot-apply sees the new timeout:
// "next request boundary" per mail-bridge-lifecycle.md § Running.
// A `0` knob falls back to defaultIdleTimeout here for the same reason
// Session.idle does (idle.go): both consumers read ONE field, so if only the
// seam-level timer applied the fallback the two would disagree about what `0`
// means — the seam would wait 29 min while the read deadline took the raw 0.
// Sweep (`value-formatting.md` § Mail-knob validation ledger).
func (s *Session) IdleTimeout() time.Duration {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.idleTimeout <= 0 {
		return defaultIdleTimeout
	}
	return s.idleTimeout
}

func (s *Session) now() time.Time {
	if s.nowFn != nil {
		return s.nowFn()
	}
	return time.Now()
}

// condStoreActive reports whether CONDSTORE response behavior is active
// for this session: either the `SELECT (CONDSTORE)` parameter was seen
// (s.condStoreEnabled) or the client `ENABLE CONDSTORE`'d — observed via
// the connection's enabled-capability set (the FAUNA-FORK enable.go patch
// routes CONDSTORE into Conn.enabled). When active, FETCH/STORE responses
// and IDLE flag-change pushes carry `MODSEQ (<n>)` per RFC 7162 §3.1.4.1.
func (s *Session) condStoreActive() bool {
	s.mu.Lock()
	enabled := s.condStoreEnabled
	conn := s.conn
	s.mu.Unlock()
	if enabled {
		return true
	}
	// EnabledCaps takes the Conn's own mutex; never hold s.mu across it.
	return conn != nil && conn.EnabledCaps().Has(imap.CapCondStore)
}

// qresyncActive reports whether QRESYNC response behavior is active for
// this session: either the `SELECT (QRESYNC ...)` parameter was seen
// (s.qresyncEnabled) or the client `ENABLE QRESYNC`'d — observed via the
// connection's enabled-capability set (the FAUNA-FORK enable.go patch
// routes QRESYNC into Conn.enabled). When active, expunge notifications
// (EXPUNGE command, IDLE push) use `* VANISHED <uid_set>` instead of the
// per-UID `* <seq> EXPUNGE` form, and a UID FETCH carrying the
// `(CHANGEDSINCE n VANISHED)` modifier emits `* VANISHED (EARLIER)` for
// the expunged set (RFC 7162 §3.2.5 / §3.2.10). QRESYNC implies
// CONDSTORE, so an active QRESYNC session also satisfies condStoreActive.
func (s *Session) qresyncActive() bool {
	s.mu.Lock()
	enabled := s.qresyncEnabled
	conn := s.conn
	s.mu.Unlock()
	if enabled {
		return true
	}
	// EnabledCaps takes the Conn's own mutex; never hold s.mu across it.
	return conn != nil && conn.EnabledCaps().Has(imap.CapQResync)
}

// ── Not-authenticated state ────────────────────────────────────────

// Login implements the IMAP `LOGIN <user> <pass>` command (RFC 9051 §
// 6.2.3). It maps to the *same* AEAD-unwrap-as-auth flow as
// AUTHENTICATE PLAIN (imap-server.md § Authentication): LOGIN-over-TLS
// carries the same cleartext credential as PLAIN-over-TLS, so there is
// no security delta, while supporting it widens MUA compatibility
// (imaplib.login(), legacy clients that only speak LOGIN). The fork
// gates LOGIN behind TLS — handleLogin checks canAuth() and the
// server advertises LOGINDISABLED pre-TLS (capabilities.go /
// AllowInsecureAuth=false) — so this method is only reached on a
// TLS-protected connection.
func (s *Session) Login(username, password string) error {
	// Reuse authenticatePLAIN so the credential_id default, actor
	// resolution, wrapped-blob fetch, AEAD unwrap, and audit event are
	// byte-for-byte identical to AUTHENTICATE PLAIN.
	if err := s.authenticatePLAIN(auth.BuildPlainPayload("", username, password)); err != nil {
		// authenticatePLAIN already fired the result=fail audit event
		// and collapsed every failure into the opaque errAuthFailed.
		// Map it to a tagged NO so the MUA sees `NO Authentication
		// failed` — a bare Go error here maps to internalServerErrorResp
		// (`NO [SERVERBUG]`) in the fork's command dispatch (conn.go).
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Authentication failed",
		}
	}
	return nil
}

// ── Authenticated state (filled in by C.5+ and Phase D) ────────────

// Session.Create / Session.Delete / Session.Rename live in
// mailbox_admin.go (Phase D.6); Session.Subscribe / Session.Unsubscribe
// live in subscribe.go (Phase D.7). Their stubs would conflict with
// the real method declarations.

func (s *Session) Status(string, *imap.StatusOptions) (*imap.StatusData, error) {
	return nil, errNotImplemented
}

// Session.Append lives in append.go (Phase D.5).

// Poll flushes any unsolicited mailbox-state updates the server has
// queued for this connection between commands. emersion calls it after
// most commands (the generic-OK path in `imapserver/conn.go` and
// explicitly from `handleAppend` / `handleCopy`), so it MUST NOT error:
// returning a non-nil error here turns an otherwise-successful APPEND /
// NOOP / FETCH into a tagged NO (see the FAUNA-FORK `Conn.poll`).
//
// The MDA delivers unsolicited updates via the push-driven IDLE path
// only (`idle.go` registers a per-IDLE `BridgeMailboxState` push
// subscription on the `notificationRouter`); there is no non-IDLE
// per-connection update queue — pushes that arrive while a session is
// not idling are dropped, not buffered (`notification_router.go`). So
// outside IDLE there is nothing for Poll to flush: it's a correct no-op.
// Per `imap-server.md` § Fallback poll ("the MDA does not fall back to
// polling — push events are reliable"). If a future track adds buffered
// non-IDLE delivery, drain it here.
func (s *Session) Poll(*imapserver.UpdateWriter, bool) error { return nil }

// Session.Idle lives in idle.go (Phase F.2). The stub would conflict
// with the real method declaration.

// ── Selected state ─────────────────────────────────────────────────

// Search lives in search.go (Phase C.7). Store lives in store.go
// (Phase D.2). Expunge in expunge.go (Phase D.3). Copy in copy.go
// and Move in move.go (Phase D.4). Append in append.go (Phase D.5).
// The stubs here would conflict with the real method declarations.

// ── SessionIMAP4rev2 (NAMESPACE + MOVE) ────────────────────────────

// Namespace returns the empty IMAP namespace. Per imap-server.md
// § Mailbox model, V1 is single-user with no shared mailboxes; the
// personal namespace's separator is `/`. RFC 9051 § 6.3.10 lets us
// return three empty namespace lists; we follow Dovecot's convention
// of reporting only the personal namespace.
func (s *Session) Namespace() (*imap.NamespaceData, error) {
	return &imap.NamespaceData{
		Personal: []imap.NamespaceDescriptor{{Prefix: "", Delim: '/'}},
	}, nil
}
