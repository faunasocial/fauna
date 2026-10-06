// Phases D.1 + D.2: SMTP submission listeners.
//
// D.1 stood up two sibling listeners sharing one backend (465 implicit
// TLS, 587 STARTTLS), TLS terminated, with stubbed AUTH rejecting all
// MAIL FROM. D.2 plumbs the real AUTH path:
//
//   - `Auth(mech)` returns sasl.NewPlainServer / sasl.NewOAuthBearerServer
//     (see this file's `Auth` method).
//   - The authenticator closures rebuild the wire payload and dispatch
//     to the workhorse at `internal/mta/auth.go::submissionSession.authenticate`,
//     which AEAD-unwraps the wrapped submission token + verifies the
//     inner Ed25519 signature against `actor_id`-as-VerifyingKey.
//   - Success stashes (actor_id, credential_id, *SubmissionTokenFfi)
//     on the session under mu and flips `authenticated`.
//   - D.3 reads the stash for MAIL FROM identity + RCPT TO quota
//     enforcement; D.6 reads it for own-submission detection.
//
// MAIL-FROM/RCPT-TO/quota, the From: header gates, outbound delivery, and the
// per-(credential, IP) auth lockout land in D.3–D.7. The door signs nothing
// and holds no DKIM key: the nest signs each message at the outbound
// hand-out. Goal doc: `docs/goal/behavior/smtp-server.md` § Auth on each
// port (PLAIN + OAUTHBEARER over TLS only; the spec's "SCRAM-SHA-256
// preferred" is a future workstream).
//
// Unlike the C.1 inboundBackend, submissionBackend skips the
// connection-time policy stack (rate-limit / DNSBL / FCrDNS / HELO
// identity) — those are inbound-perimeter defenses; the submission
// path is auth-gated and gets a separate per-(credential, IP) lockout
// in Phase D.7.
package mta

import (
	"bytes"
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"strings"
	"sync"
	"time"

	"github.com/emersion/go-sasl"
	gosmtp "github.com/emersion/go-smtp"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/auth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/bridgeshutdown"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// submissionBackend is the gosmtp.Backend for the 465 + 587 listeners.
//
// `cfg` is the hot-swappable MTA config holder shared with the inbound
// backend (one per process). It carries the deployment's active
// mail_domains list (D.3's MAIL FROM validation checks the authenticated
// actor's MAIL FROM domain is in it, per docs/goal/behavior/mail-multidomain.md),
// the primary-domain anchor (outbound EHLO host + canonical sender identity
// when building the authedAddress), and
// the D.7 per-credential AUTH lockout. Each session reads the current bundle
// at its request boundary so an admin `config_changed` edit hot-applies with
// no restart (mail-policy-config.md § Architectural rules).
// `logger` is the per-listener slog handle (typically with a
// listener= attribute attached upstream).
// `client` is the WS-RPC connection to nest, used by D.2's AUTH path
// for `validate_recipient`, `fetch_wrapped_submission_token`, and
// `report_auth_event`. `nowFn` is overridable in tests so the
// audit-log occurred_at assertions don't depend on wallclock.
type submissionBackend struct {
	cfg    *mtaConfigHolder
	logger *slog.Logger
	client wsrpc.Caller
	nowFn  func() time.Time
	// bytePlane stages an oversized sealed body on the nest's bulk-byte plane,
	// so an authenticated submission above the 2 MiB WS-RPC frame still delivers
	// (smtp-server.md § Message size limits). Same client the inbound backend
	// uses; nil in unit tests that never seal an over-budget body.
	bytePlane *byteplane.Client
	// outboundTrigger pokes the outbound worker after a successful
	// enqueue so the row gets drained without waiting for the next
	// poll-interval tick (Phase D.5). nil when no worker is wired
	// (test fixtures, plus the Phase E carryover where the worker
	// isn't yet started); Data() then enqueues and returns 250 without
	// the early-drain optimisation.
	outboundTrigger func()
	// drain coordinates graceful shutdown (T2.6): NewSession/Logout track
	// in-flight submissions and Mail answers 421 4.3.2 once draining. nil in
	// test fixtures (the gate then no-ops).
	drain *drainTracker
}

// now returns the backend's wall-clock or test-injected time. Mirrors
// the IMAP backend's same-named helper so the audit-log occurred_at
// path is testable without sleeping.
func (b *submissionBackend) now() time.Time {
	if b.nowFn != nil {
		return b.nowFn()
	}
	return time.Now()
}

// NewSession returns a fresh submission session per accepted
// connection. No connection-time policy runs here — submission has its
// own auth lockout in D.7; the inbound-perimeter gates from C.2 are
// inbound-only.
//
// The session captures the peer source-IP for D.7's per-(credential,
// IP) lockout AND for the audit-log entries fired during AUTH (success
// or fail). Tests that drive the session directly (no real TCP) can
// leave `c.Conn()` nil; the `RemoteAddr()` guard then yields an empty
// `sourceIP` and the audit-log code path treats that as "unavailable"
// rather than crashing.
func (b *submissionBackend) NewSession(c *gosmtp.Conn) (gosmtp.Session, error) {
	if b.drain != nil {
		b.drain.enter()
	}
	sess := &submissionSession{backend: b}
	if c != nil && c.Conn() != nil {
		if addr, ok := c.Conn().RemoteAddr().(*net.TCPAddr); ok {
			sess.sourceIP = addr.IP.String()
		} else if c.Conn().RemoteAddr() != nil {
			// Non-TCP listener (e.g. unix socket in tests): fall back
			// to the full address string.
			sess.sourceIP = c.Conn().RemoteAddr().String()
		}
	}
	return sess, nil
}

// submissionSession is the per-connection state for one submission.
//
// Mutex-guarded mutation matches the IMAP Session shape: AUTH success
// from one goroutine MUST be visible to Mail/Rcpt/Data on the same
// connection (gosmtp serializes per-session calls, but the
// sasl.Server callback runs in its own goroutine before handing back
// to the dispatcher).
type submissionSession struct {
	backend *submissionBackend
	mu      sync.Mutex

	// loggedOut guards the single drain.leave() — go-smtp may call Logout
	// from more than one teardown path (T2.6 graceful shutdown).
	loggedOut sync.Once

	// authenticated is set by D.2's PLAIN / OAUTHBEARER workhorse
	// after a successful AEAD-unwrap of the wrapped submission token.
	// D.3 gates Mail/Rcpt on it.
	authenticated bool

	// Set by D.2 AUTH:
	actorID         []byte
	credentialID    string
	authedLocalPart string
	// submissionToken holds the unwrapped policy payload — quotas,
	// expiry, max-recipients — that D.3 (MAIL FROM + RCPT TO +
	// quota) and D.7 (per-credential lockout) consume. Holds no
	// secret material, so no Zeroize on session close (unlike the
	// IMAP MLSCapability path).
	submissionToken *mailfauna.SubmissionTokenFfi

	// sourceIP is the peer's IP captured at accept-time. Empty when
	// the connection is non-TCP (test fixtures); audit-log entries
	// then carry an empty source_ip which nest's
	// `report_auth_event` rejects in production but tolerates with
	// a stub caller.
	sourceIP string

	// recipientCount tracks the running RCPT TO count for the current
	// transaction; cleared on each MAIL FROM and on RSET (D.3). The
	// per-message fast-path compares it against
	// submissionToken.MaxRecipients; the authoritative path is
	// fauna.bridges.check_submission_quota which decides against
	// nest's per-actor windowed quota (1000 recipients/day in
	// smtp-server.md § Architectural rules).
	recipientCount uint32

	// recipients accumulates every RCPT TO that passed validation in
	// the current transaction. Data() hands the full list to
	// fauna.bridges.enqueue_outbound_mail so each recipient gets its
	// own retry curve in `outbound_mail_queue`. Cleared on MAIL FROM /
	// Reset, same lifecycle as recipientCount. Data() partitions this
	// into Fauna vs external before calling enqueue + submit_inbound_mail;
	// external recipients route through enqueue.
	recipients []string

	// localResolved holds the local-domain recipients that passed
	// RCPT-time resolution: resolve_recipient resolved each to a local
	// mailbox actor (with an MLS pubkey on file) or an admin external
	// forwarder. Populated in Rcpt, consumed in Data. Resolving local
	// recipients at RCPT TO (matching
	// the inbound resolver timing in mail-aliases.md § Resolution order)
	// is what lets one invalid recipient reject per-RCPT (550) without
	// failing delivery to the others — see smtp-server.md § Recipient
	// handling on submission. Cleared on MAIL FROM / Reset, same
	// lifecycle as recipients.
	localResolved []resolvedLocalRcpt

	// envelopeFrom is the MAIL FROM address the session validated as
	// owned by the authenticated actor (canonical `localpart@domain`,
	// domain lower-cased). Set on each accepted MAIL FROM, consumed by
	// Data() as the outbound envelope sender (Return-Path / bounce
	// address). Unlike the pre-alias behaviour — which recomputed the
	// sender to `<handle>@<primary-domain>` — this preserves the exact
	// owned address the user submitted (a primary handle OR any owned
	// alias on any local domain), per mail-multidomain.md § Cross-domain
	// submission policy. Cleared on Reset, same transaction lifecycle as
	// recipients.
	envelopeFrom string
}

// resolvedLocalRcpt is a local-domain RCPT TO that passed RCPT-time
// resolution via resolve_recipient (the fixed-order alias resolver,
// mail-aliases.md § Resolution order). Two shapes:
//
//   - A local mailbox (the common case): the resolver returned `Resolved`,
//     `addr` resolved to `actorID`, the recipient has `mlsPubkey` on file, and
//     `headersToStamp` carries any matched-alias X-Fauna-Address-* headers
//     (subaddress / wildcard / disposable / catch-all — empty for exact /
//     role-address). Data seals the body (with the stamp prepended) to mlsPubkey
//     and routes via submit/ingest_inbound_mail. `isRoleAddress` records a
//     postmaster@/abuse@/… route (bypasses the recipient's per-mailbox quota on
//     ingest, smtp-server.md :204).
//   - An admin external forwarder (`forward` set): the resolver returned
//     `Forward` (mail-aliases.md § Kind 7) — a mailbox-less address that
//     redirects to `forwardTarget`, attributed to the managing admin
//     `forwarderActorID`. Data redirects it through the shared forward dispatch
//     (copy_mode=redirect, no local copy); `actorID`/`mlsPubkey` are nil.
type resolvedLocalRcpt struct {
	addr      string
	actorID   []byte
	mlsPubkey []byte
	// mlkemEk is the ML-KEM half of the recipient's seal key (1184 B), set
	// whenever mlsPubkey is. Cached alongside it at RCPT time so the body
	// seals X-Wing without a second fetch (post-quantum S3d).
	mlkemEk        []byte
	isRoleAddress  bool
	headersToStamp []wsrpc.StampedHeader

	// forward marks an admin external-forwarder match: redirect to
	// forwardTarget attributed to forwarderActorID, no local copy.
	forward          bool
	forwardTarget    string
	forwarderActorID []byte
}

// now is a session-level convenience that forwards to the backend's
// time source — mirrors the IMAP Session.now() shape so test code can
// inject a fixed clock once at backend creation.
func (s *submissionSession) now() time.Time {
	return s.backend.now()
}

// AuthMechanisms advertises the SASL mechanisms the listener
// supports — wired to D.2's PLAIN + OAUTHBEARER workhorse via
// `Auth(mech)` below.
func (s *submissionSession) AuthMechanisms() []string {
	return []string{sasl.Plain, sasl.OAuthBearer}
}

// Auth returns the SASL server gosmtp drives for AUTH <mech>. The
// returned server's authenticator closure rebuilds the canonical
// payload via internal/auth helpers and dispatches to the workhorse
// in auth.go.
//
// Mirrors `internal/mda/imap/auth.go::Session.Authenticate` shape down
// to the closure pattern; the only differences are the per-protocol
// error type (gosmtp.SMTPError vs the IMAP NO mapping) and the
// per-protocol "fail" wire response (RFC 4954 535 5.7.8 vs IMAP NO).
func (s *submissionSession) Auth(mech string) (sasl.Server, error) {
	// The SASL servers parse the wire payload, hand us the parsed
	// credentials via the authenticator closure, and we rebuild the
	// canonical payload via `internal/auth` helpers so the workhorse
	// in `auth.go` can re-parse with the same code that test fixtures
	// use. Rebuild-then-reparse keeps the workhorse driveable from
	// tests without a SASL pipeline.
	switch mech {
	case sasl.Plain:
		return sasl.NewPlainServer(func(identity, username, password string) error {
			if identity != "" && identity != username {
				// RFC 4616: authzid empty or equal to authcid.
				return errSubmissionAuthFailed
			}
			return s.authenticate(sasl.Plain, auth.BuildPlainPayload(identity, username, password))
		}), nil
	case sasl.OAuthBearer:
		return sasl.NewOAuthBearerServer(func(opts sasl.OAuthBearerOptions) *sasl.OAuthBearerError {
			payload := auth.BuildOAuthBearerPayload(opts.Username, opts.Token)
			if err := s.authenticate(sasl.OAuthBearer, payload); err != nil {
				// RFC 7628 § 3.2.2: invalid_token covers both
				// "bad token format" and "token rejected".
				return &sasl.OAuthBearerError{
					Status:  "invalid_token",
					Schemes: "bearer",
				}
			}
			return nil
		}), nil
	default:
		return nil, &gosmtp.SMTPError{
			Code:         504,
			EnhancedCode: gosmtp.EnhancedCode{5, 5, 4},
			Message:      "AUTH mechanism not supported",
		}
	}
}

// Mail validates the envelope sender against the addresses the
// authenticated actor owns (D.3), per docs/goal/behavior/mail-multidomain.md
// § Cross-domain submission policy. Two conditions must hold:
//
//   - The MAIL FROM domain MUST be one of the deployment's active
//     local_domains — ANY of them, not just the primary (the deployment
//     is closed-relay; we don't accept submission for non-local domains).
//     A non-local domain returns 553 5.7.1.
//   - The MAIL FROM local-part MUST resolve to an address the
//     authenticated actor owns. The authenticated login handle
//     (s.authedLocalPart) is always owned, so it takes a cheap fast-path
//     with no RPC. Any OTHER local-part (an alias the user chose in their
//     MUA's "From" selector — e.g. macOS Mail lets the user pick any of
//     their addresses) is resolved via fauna.bridges.resolve_recipient
//     and accepted only when it resolves to the authenticated actor
//     (spec: the MTA compares the resolver's actor_id with the
//     authenticated actor). A local-part the actor does not own returns
//     550 5.7.1 "Sender not authorized for this address"; a resolver
//     transport failure returns 451 4.7.1 (never smuggle past a gate we
//     can't evaluate).
//
// MAIL FROM:<> (the empty null sender, reserved for server-generated
// DSNs per RFC 5321 §3.3) cannot match an authenticated user and is
// rejected with 553. A syntactically malformed reverse-path gets the
// more specific 501 5.5.2 so MUAs distinguish "I sent garbage" from
// "you're sending as the wrong identity".
//
// The From: header (what the MUA displays and the DKIM d= is keyed on) is not
// alignment-checked against the envelope — per spec § From: vs. MAIL FROM:
// the two are independent values — but it is OWNERSHIP-checked in Data(),
// through the same predicate (assertSenderOwned): a From: on a domain the
// deployment signs for must name an address the authenticated actor owns
// (mail-multidomain.md § From: header ownership). The From: header's
// domain-locality is enforced in checkFromLocal (550 5.7.7 for a non-local
// From domain, when the deployment signs outbound mail).
//
// On accept, the validated address is stashed as the outbound envelope
// sender (envelopeFrom) and recipientCount resets — a new MAIL FROM
// begins a new transaction with a fresh per-message recipient cap.
func (s *submissionSession) Mail(from string, _ *gosmtp.MailOptions) error {
	// Graceful shutdown (mail-bridge-lifecycle.md § Shutting down step 2):
	// refuse new submissions with 421 4.3.2 once the bridge is draining.
	if s.backend != nil && s.backend.drain != nil && s.backend.drain.isDraining() {
		return &gosmtp.SMTPError{
			Code:         421,
			EnhancedCode: gosmtp.EnhancedCode{4, 3, 2},
			Message:      "Service shutting down",
		}
	}
	s.mu.Lock()
	authed := s.authenticated
	authedLocal := s.authedLocalPart
	actorID := s.actorID
	s.mu.Unlock()

	if !authed {
		return &gosmtp.SMTPError{
			Code:         530,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 0},
			Message:      "Authentication required",
		}
	}
	if from == "" {
		return &gosmtp.SMTPError{
			Code:         553,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
			Message:      "MAIL FROM identity does not match authenticated user",
		}
	}
	localPart, domain, err := splitRcptAddress(from)
	if err != nil {
		return &gosmtp.SMTPError{
			Code:         501,
			EnhancedCode: gosmtp.EnhancedCode{5, 5, 2},
			Message:      "MAIL FROM syntax error: " + err.Error(),
		}
	}
	// Domain locality (RFC 5321 §2.4 case-insensitive). Closed-relay:
	// submission is only for domains we host.
	if !containsFoldDomain(s.backend.cfg.current().localDomains, domain) {
		return &gosmtp.SMTPError{
			Code:         553,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
			Message:      "MAIL FROM domain is not a local domain",
		}
	}
	// Local-part ownership — the one sender-ownership predicate, shared
	// with Data()'s From: header check (mail-multidomain.md § Cross-domain
	// submission policy → Required invariants).
	if err := s.assertSenderOwned(localPart, domain, authedLocal, actorID); err != nil {
		return err
	}

	s.mu.Lock()
	s.envelopeFrom = localPart + "@" + strings.ToLower(domain)
	s.recipientCount = 0
	s.recipients = nil
	s.localResolved = nil
	s.mu.Unlock()
	return nil
}

// assertSenderOwned is THE sender-ownership predicate of the submission
// door: the one rule both sender identities — the envelope MAIL FROM (Mail)
// and the RFC 5322 From: header (Data) — are held to, so the two can never
// drift apart (mail-multidomain.md § From: header ownership). `localPart@domain` is owned by the authenticated actor iff:
//
//   - domain is an ACTIVE local domain, exactly — a subdomain the signer would
//     key under its parent hosts no mailbox, so nothing on it can be owned
//     (550 5.7.1); and
//   - localPart is the authenticated login handle, compared
//     case-insensitively (the handle is owned on every active local domain —
//     mail-multidomain.md § Multi-domain handles — and validate_recipient
//     returned it canonical at AUTH time, so no RPC is needed); or
//   - localPart is an alias resolve_recipient attributes to this actor
//     (assertMailFromOwned; 550 5.7.1 when not, 451 4.7.1 when unverifiable).
func (s *submissionSession) assertSenderOwned(localPart, domain, authedLocal string, actorID []byte) error {
	if !containsFoldDomain(s.backend.cfg.current().localDomains, domain) {
		return &gosmtp.SMTPError{
			Code:         550,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
			Message:      "Sender not authorized for this address",
		}
	}
	if strings.EqualFold(localPart, authedLocal) {
		return nil
	}
	return s.assertMailFromOwned(localPart, domain, actorID)
}

// assertMailFromOwned verifies that a sender address whose local-part differs
// from the authenticated login handle is nonetheless an address the
// authenticated actor owns, by resolving it through
// fauna.bridges.resolve_recipient and comparing the owning actor_id
// with actorID (mail-multidomain.md § Cross-domain submission policy). The
// resolver arm of assertSenderOwned, which is what the doors call.
// Returns nil when owned; otherwise a *gosmtp.SMTPError:
//
//   - 451 4.7.1 on a resolver transport failure OR a missing nest client
//     (fail closed — we cannot verify ownership, so we don't accept).
//   - 550 5.7.1 when the address does not resolve to a local mailbox
//     owned by the authenticated actor (an unowned exact/alias/role/
//     catch-all address, an external forwarder, or an unresolvable
//     recipient).
func (s *submissionSession) assertMailFromOwned(localPart, domain string, actorID []byte) error {
	client := s.backend.client
	if client == nil {
		// No nest wired (test fixture / pre-approval): ownership is
		// unverifiable. Fail closed rather than accept an unchecked alias.
		return &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 7, 1},
			Message:      "Sender validation temporarily unavailable; try again later",
		}
	}
	// sender_domain is audit-log context only (does not affect resolution);
	// mirror the RCPT resolver and pass the primary-domain anchor.
	senderDomain := s.backend.cfg.current().primaryDomain
	ctx, cancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
	// Empty sender_address: this resolves the MAIL FROM address itself, to check
	// the authenticated actor owns it — it is not a delivery to a recipient, so
	// the nest's guardian mail gate must not weigh in (an empty address reads as
	// a known sender and is never rejected).
	res, err := wsrpc.ResolveRecipient(ctx, client, localPart, domain, senderDomain, "")
	cancel()
	if err != nil {
		if s.backend.logger != nil {
			s.backend.logger.Warn("mta: MAIL FROM resolve_recipient transport error",
				"err", err, "mail_from", localPart+"@"+domain)
		}
		return &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 7, 1},
			Message:      "Sender validation temporarily unavailable; try again later",
		}
	}
	// Only a Resolved local mailbox owned by THIS actor authorises the
	// sender. Forward (an admin external forwarder — no sending mailbox),
	// Reject, Discard, or a mismatched actor_id are all "not yours".
	if res.Outcome != wsrpc.ResolveResolved || !bytes.Equal(res.ActorID, actorID) {
		return &gosmtp.SMTPError{
			Code:         550,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 1},
			Message:      "Sender not authorized for this address",
		}
	}
	return nil
}

// Rcpt validates the recipient and enforces the two-tier recipient
// quota.
//
// **Recipient validation (smtp-server.md § Recipient handling on
// submission).** A local-domain recipient (domain ∈ localDomains) is
// resolved here, at RCPT TO, via resolveLocalRecipient: resolve_recipient
// must resolve it to a local mailbox (with an MLS pubkey on file) or an
// admin external forwarder. Reject → 550 5.1.1 on *this* RCPT only (gosmtp
// drops it from the envelope; other recipients are unaffected). Transport
// error → 451 4.7.1. Resolving up front is what lets one bad local address
// reject per-recipient without failing delivery to the others. External
// recipients can't be validated at submission time (the remote mailbox
// is unknown) — they're accepted here and ride the outbound queue + NDR.
// A rejected recipient returns *before* the quota counter moves, so it
// never counts against the per-message cap.
//
// **Quota:**
//  1. **Fast-path** — bump recipientCount; reject 452 4.7.12 if the
//     per-message MaxRecipients on the unwrapped submission token has
//     been exceeded. No RPC; the token's MaxRecipients is the
//     authenticated-at-token-issue ceiling.
//  2. **Authoritative path** — call
//     `fauna.bridges.check_submission_quota(actor_id, recipient_count)`.
//     Nest decides against its per-actor windowed quota (default 1000
//     recipients/day per smtp-server.md § Architectural rules).
//     over_quota → 452 4.7.12. Transport / decode failure → 451 4.7.0
//     (don't smuggle past a gate we can't evaluate).
//
// Both quota signals are kept on purpose: the token's MaxRecipients is
// a cheap pre-RPC floor that bounds the *one-message* fan-out; nest's
// reply is the source of truth for cross-message accumulation.
func (s *submissionSession) Rcpt(to string, _ *gosmtp.RcptOptions) error {
	s.mu.Lock()
	authed := s.authenticated
	actorID := s.actorID
	var maxRcpt uint32
	if s.submissionToken != nil {
		maxRcpt = s.submissionToken.MaxRecipients
	}
	s.mu.Unlock()

	if !authed {
		return &gosmtp.SMTPError{
			Code:         530,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 0},
			Message:      "Authentication required",
		}
	}

	// RCPT-time resolution for local-domain recipients. Runs before the
	// quota counter moves so a rejected recipient never counts.
	var resolved *resolvedLocalRcpt
	if localPart, domain, perr := splitRcptAddress(to); perr == nil && containsFoldDomain(s.backend.cfg.current().localDomains, domain) {
		r, rerr := s.resolveLocalRecipient(to, localPart, domain)
		if rerr != nil {
			return rerr
		}
		resolved = r
	}

	s.mu.Lock()
	s.recipientCount++
	rc := s.recipientCount
	// Record the address for Data's enqueue path; reverting on quota
	// rejection below keeps the stored list consistent with what
	// gosmtp considers accepted (a 4xx/5xx return undoes the RCPT TO
	// per RFC 5321 §4.1.1.3).
	s.recipients = append(s.recipients, to)
	s.mu.Unlock()

	if maxRcpt > 0 && rc > maxRcpt {
		s.revertLastRecipient()
		return &gosmtp.SMTPError{
			Code:         452,
			EnhancedCode: gosmtp.EnhancedCode{4, 7, 12},
			Message:      "Per-message recipient limit exceeded",
		}
	}

	if client := s.backend.client; client != nil {
		// One call per accepted RCPT: `rc` feeds nest's per-message cap, and
		// the locality bit decides whether THIS recipient costs a unit of the
		// daily allowance — a mailbox on this deployment costs nothing, an
		// outside address or an admin external forwarder (the message leaves
		// through the forward dispatch) costs one (smtp-server.md
		// § Architectural rules, the charging rule).
		recipientIsLocal := resolved != nil && !resolved.forward
		ctx, cancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
		allowed, _, err := wsrpc.CheckSubmissionQuota(ctx, client, actorID, rc, recipientIsLocal)
		cancel()
		if err != nil {
			if s.backend.logger != nil {
				s.backend.logger.Warn("mta: check_submission_quota transport error",
					"err", err, "recipient_count", rc)
			}
			s.revertLastRecipient()
			return &gosmtp.SMTPError{
				Code:         451,
				EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
				Message:      "Submission quota check temporarily unavailable; try again later",
			}
		}
		if !allowed {
			s.revertLastRecipient()
			return &gosmtp.SMTPError{
				Code:         452,
				EnhancedCode: gosmtp.EnhancedCode{4, 7, 12},
				Message:      "Per-actor submission quota exceeded",
			}
		}
	}

	// Commit the resolved local recipient only after the quota gates
	// pass, so a quota-rejected RCPT never leaves a stale entry for
	// Data to deliver to.
	if resolved != nil {
		s.mu.Lock()
		s.localResolved = append(s.localResolved, *resolved)
		s.mu.Unlock()
	}
	return nil
}

// resolveLocalRecipient resolves a local-domain RCPT at RCPT TO time through
// resolve_recipient — the fixed-order alias resolver (mail-aliases.md
// § Resolution order: exact → forwarder → +suffix → disposable → wildcard →
// role-address → catch-all → 550), the superset of the AUTH-time exact-only
// validate_recipient. Three outcomes:
//
//   - Resolved → a local mailbox: the actor must have an MLS pubkey on file
//     (else 550 5.1.1 no-encryption-key — or 451 4.7.1 when the actor is a
//     succession's successor still inside the bounded re-provisioning
//     window, smtp-server.md § Error / tempfail strategy). Carries the
//     matched-alias headers_to_stamp + the role-address bit.
//   - Forward → an admin external forwarder (mail-aliases.md § Kind 7): no local
//     mailbox, no pubkey fetch — Data redirects it to the external target.
//   - Reject → mapped to the SMTP wire via rejectFromResolver (550 user-unknown
//     / disabled / expired; 451 once the bridge enforces caps).
//
// A nil nest client (test fixtures that spin the listener without a wired nest)
// skips resolution — Data() 451s before any delivery in that case, so there is
// nothing to resolve here.
func (s *submissionSession) resolveLocalRecipient(addr, localPart, domain string) (*resolvedLocalRcpt, error) {
	client := s.backend.client
	if client == nil {
		return nil, nil
	}
	// sender_domain is the authenticated user's canonical domain (audit-log only,
	// does not affect resolution); Mail() pinned the MAIL FROM to a local domain.
	senderDomain := s.backend.cfg.current().primaryDomain
	// The submitter's envelope address feeds the nest's guardian mail gate: this
	// is the **in-domain twin** of the inbound `RCPT TO` reject — an external MUA
	// mailing a ward whose guardian set `unknown_sender_mail = reject` is refused
	// here, per-recipient, so a co-recipient adult still receives their copy
	// (`family-safety.md` § The mail gate).
	s.mu.Lock()
	senderAddress := s.envelopeFrom
	s.mu.Unlock()
	ctx, cancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
	res, err := wsrpc.ResolveRecipient(ctx, client, localPart, domain, senderDomain, senderAddress)
	cancel()
	if err != nil {
		// Transport / decode / unknown-outcome — tempfail (never silently accept
		// undeliverable mail; same posture as the inbound resolver path).
		if s.backend.logger != nil {
			s.backend.logger.Warn("mta: resolve_recipient transport error", "err", err, "rcpt", addr)
		}
		return nil, &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 7, 1},
			Message:      "Recipient validation temporarily unavailable; try again later",
		}
	}
	switch res.Outcome {
	case wsrpc.ResolveReject:
		return nil, rejectFromResolver(res.SMTPCode, res.Reason)
	case wsrpc.ResolveForward:
		// Admin external forwarder: no local mailbox → no MLS pubkey to fetch.
		// Data redirects to the external target (copy_mode=redirect), attributed
		// to the managing admin.
		return &resolvedLocalRcpt{
			addr:             addr,
			forward:          true,
			forwardTarget:    res.ForwardTarget,
			forwarderActorID: res.ForwarderActorID,
		}, nil
	case wsrpc.ResolveResolved:
		// fall through to the MLS-pubkey confirmation below.
	default:
		// Defensive: ResolveRecipient already errors on an unknown outcome.
		return nil, &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 7, 1},
			Message:      "Recipient validation temporarily unavailable; try again later",
		}
	}
	ctx, cancel = context.WithTimeout(context.Background(), auth.RPCTimeout)
	// Own-submission recipient validation feeds the eventual body seal — a
	// genuine mail-new-ingest resolution (content-sealing-epochs).
	// checkRecipientMLSPubkeyAtRcpt (fauna_recipient.go) owns the succession-
	// pending-vs-never-onboarded distinction (smtp-server.md § Error / tempfail
	// strategy) — shared with the inbound MX arm's own RCPT-time check.
	mlsPubkey, mlkemEk, smtpErr := checkRecipientMLSPubkeyAtRcpt(ctx, client, res.ActorID, addr, true, s.backend.logger)
	cancel()
	if smtpErr != nil {
		return nil, smtpErr
	}
	return &resolvedLocalRcpt{
		addr:           addr,
		actorID:        res.ActorID,
		mlsPubkey:      mlsPubkey,
		mlkemEk:        mlkemEk,
		isRoleAddress:  res.IsRoleAddress,
		headersToStamp: res.HeadersToStamp,
	}, nil
}

// dispatchForwarderRedirect redirects a submitted message addressed to an admin
// external forwarder (mail-aliases.md § Kind 7) to its external target through
// the shared forward dispatch — attributed to the managing admin, copy_mode=
// redirect, no local copy. This makes a local user emailing a forwarder address
// (info@<our-domain>) reach the external destination uniformly with inbound MX
// delivery (mail-forwarding.md § Admin external forwarders). The original sender
// is the authenticated user (never null — Mail() rejects MAIL FROM:<>), so the
// nest-side SRS rewrite at queue-out encodes the user as the original sender and
// routes any bounce to the admin forwarder owner. Returns the submission's 451
// when nest did not take the forward (the forwarder keeps no local copy, so a
// 250 would lose the message for that recipient — mail-forwarding.md § Queue
// ceiling); a floor's deliberate suppression returns nil.
func (s *submissionSession) dispatchForwarderRedirect(parsed *mailfauna.ParsedMessage, raw []byte, from string, rcpt resolvedLocalRcpt) *gosmtp.SMTPError {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	// original_msgid: the submitted Message-ID when present; else a fresh queue
	// id so nest's non-empty-msgid check passes (mirrors the inbound redirect).
	origMsgID := strings.TrimSpace(parsed.MessageID)
	if origMsgID == "" {
		origMsgID = mailfauna.NewQueueID()
	}
	if dispatchForward(ctx, s.backend.client, s.backend.logger, s.backend.outboundTrigger,
		s.sourceIP, from, parsed.Headers, raw, rcpt.forwarderActorID, rcpt.forwardTarget,
		origMsgID, "forwarder", wsrpc.ForwardCopyModeRedirect) == forwardFailed {
		return forwarderTempfailError()
	}
	return nil
}

// revertLastRecipient drops the most recently appended recipient when
// a quota gate rejects the RCPT TO. Mirrors gosmtp's semantics: a
// 4xx/5xx return from Rcpt removes that recipient from the envelope.
// recipientCount is left intact so the per-message ceiling remains a
// monotonic counter (matches the legacy bridge-smtp behaviour).
func (s *submissionSession) revertLastRecipient() {
	s.mu.Lock()
	if n := len(s.recipients); n > 0 {
		s.recipients = s.recipients[:n-1]
	}
	s.mu.Unlock()
}

// Data reads the submitted message body, runs the From: header gates,
// then splits the recipients between the Fauna-recipient path (D.6 —
// HPKE-seal + submit_inbound_mail/ingest_inbound_mail) and the
// external MX path (D.5 — enqueue_outbound_mail) before returning
// 250 OK.
//
// Routing summary:
//   - Local-domain recipients were resolved at RCPT TO (Rcpt →
//     resolveLocalRecipient); Data reads the resolved (actor_id,
//     mls_pubkey) from the session cache and seals an encrypted copy to
//     each via ingest_inbound_mail (or submit_inbound_mail if it equals
//     the sender's own actor — which only happens when the sender names
//     themselves in TO/CC, and dedupLocalRecipients folds it into the
//     always-emitted Sent-copy path).
//   - Recipients whose domain does not match are external; they ride
//     enqueue_outbound_mail as before.
//   - A Sent copy for the sender's own actor is always emitted via
//     submit_inbound_mail. The nest handler's is_own_submission=true
//     branch routes it to the Sent mailbox (\Seen).
//
// Because recipient existence + key-availability are resolved at RCPT
// TO, one invalid recipient is rejected per-RCPT (550) and never
// reaches Data — see smtp-server.md § Recipient handling on submission.
//
// Failure modes:
//   - Unauthenticated → 530 5.7.0.
//   - Empty RCPT list (defensive, gosmtp normally fences) → 554 5.5.1.
//   - Body read error → 451 4.5.0.
//   - From: header domain not local → 550 5.7.7 (checkFromLocal).
//   - RFC 5322 parse error on the body → 451 4.5.0 (the body already
//     passed the From: gates; if it doesn't parse, something is wrong
//     internally — bounce-class would be wrong since the sender's
//     payload was accepted at upload time).
//   - DATA-time delivery residual (HPKE-seal error, ingest/submit
//     transport error) → 451 4.7.0. With existence + key resolved at
//     RCPT, only genuinely-transient failures remain here; the MUA
//     retries the whole DATA.
//   - enqueue_outbound_mail transport error → 451 4.5.0.
//
// ARC seal-on-relay is documented as a goal-doc requirement
// (smtp-server.md:16) but the submission path never carries prior
// Authentication-Results (user-composed mail is origin-side, not
// relayed). ARC wiring lands when the relay-path caller does in
// Phase E or later — the libs/fauna-mail::outbound::arc::seal
// primitive is already lifted (D.4) so that future wire-up is a
// one-call addition.
func (s *submissionSession) Data(r io.Reader) error {
	s.mu.Lock()
	authed := s.authenticated
	authedLocal := s.authedLocalPart
	actorID := s.actorID
	rcpts := append([]string(nil), s.recipients...)
	localResolved := append([]resolvedLocalRcpt(nil), s.localResolved...)
	// The outbound envelope sender is the address the session validated as
	// owned at MAIL FROM (a primary handle OR an owned alias on any local
	// domain — mail-multidomain.md § Cross-domain submission policy). Fall
	// back to the canonical handle@primary form only if a fixture drove
	// Data() without a preceding MAIL FROM (envelopeFrom then empty).
	sender := s.envelopeFrom
	if sender == "" {
		sender = s.authedFromAddress()
	}
	s.mu.Unlock()
	if !authed {
		return &gosmtp.SMTPError{
			Code:         530,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 0},
			Message:      "Authentication required",
		}
	}
	if len(rcpts) == 0 {
		// No accepted RCPT TO before DATA. gosmtp normally fences this
		// (DATA without RCPT is rejected at the protocol layer), but
		// defend cleanly in case a fixture drives Data directly.
		return &gosmtp.SMTPError{
			Code:         554,
			EnhancedCode: gosmtp.EnhancedCode{5, 5, 1},
			Message:      "No recipients accepted",
		}
	}

	// Pre-parser size guard (§ B6): bound io.ReadAll with a LimitReader so an
	// authenticated submitter can't force a multi-GiB transient — the body is
	// re-copied 2–3× downstream (strip → stamp → parse). The cap is the same
	// snapshot-driven `max_message_bytes` (mail policy, default 50 MB) the inbound
	// MX path enforces (server.go inboundSession.Data); a `0` cap (tests / no
	// snapshot) reads unbounded, matching that path. Mirrors the inbound 552
	// size-limit reply rather than reading then erroring.
	maxBytes := s.backend.cfg.current().maxMessageBytes
	var raw []byte
	var err error
	if maxBytes > 0 {
		raw, err = io.ReadAll(io.LimitReader(r, int64(maxBytes)+1))
	} else {
		raw, err = io.ReadAll(r)
	}
	if err != nil {
		if s.backend.logger != nil {
			s.backend.logger.Warn("mta: submission DATA read failed", "err", err)
		}
		return &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 5, 0},
			Message:      "Failed to read message body; try again later",
		}
	}
	if maxBytes > 0 && uint32(len(raw)) > maxBytes {
		if s.backend.logger != nil {
			s.backend.logger.Info("mta: submission exceeds size limit",
				"bytes", len(raw),
				"cap", maxBytes,
				"source_ip", s.sourceIP,
			)
		}
		return &gosmtp.SMTPError{
			Code:         552,
			EnhancedCode: gosmtp.EnhancedCode{5, 3, 4},
			Message:      "Message exceeds fixed size limit",
		}
	}

	// RFC 5322 §3.6: exactly one From field (smtp-server.md § Architectural
	// rules → Exactly one From field). checkFromLocal makes the local-domain
	// check — and the nest picks the DKIM key — by the LAST From field, while a
	// receiver's DMARC may align against the first — refuse before stripping,
	// filing or enqueuing anything.
	if smtpErr, n := checkFromFieldCount(raw); smtpErr != nil {
		if s.backend.logger != nil {
			s.backend.logger.Info("mta: submission rejected — not exactly one From header field",
				"from_fields", n,
				"source_ip", s.sourceIP,
			)
		}
		return smtpErr
	}

	// From: header ownership (mail-multidomain.md § From: header
	// ownership). The signer keys the DKIM d= on this
	// header's domain, so a From: naming another local user would leave
	// DMARC-aligned FOR THAT USER — the envelope check above never reads
	// it. Two gates, before the strip / file / enqueue chain, and
	// independent of whether the deployment signs at all (checkFromLocal
	// skips its check when it does not):
	//
	//  1. the one From field names exactly one mailbox (RFC 5322 §3.6.2
	//     allows a list; RFC 7489 §6.6.1 names rejection for it) — two would
	//     sign under the first's domain with a foreign one riding along, none
	//     leaves nothing to own: 554 5.6.0, the field-count refusal's code;
	//  2. when the deployment would sign for its domain (an active local
	//     domain or a subdomain of one — SelectSigningDomain, the same rule
	//     checkFromLocal keys on), that mailbox must be owned by the
	//     authenticated actor: assertSenderOwned, the predicate MAIL FROM
	//     passed. An off-domain From: is not ours to vouch for and stays on
	//     checkFromLocal's 550 5.7.7.
	//
	// The list and the signer's anchor come from one shared-Rust reader
	// (fauna_mail::envelope::from_mailboxes / sender_domain), so the door and
	// the signer can never disagree about which address the message is from.
	fromMailboxes := mailfauna.FromMailboxes(raw)
	if len(fromMailboxes) != 1 {
		if s.backend.logger != nil {
			s.backend.logger.Info("mta: submission rejected — From field must name exactly one mailbox",
				"from_mailboxes", len(fromMailboxes),
				"source_ip", s.sourceIP,
			)
		}
		return &gosmtp.SMTPError{
			Code:         554,
			EnhancedCode: gosmtp.EnhancedCode{5, 6, 0},
			Message:      "From field must name exactly one mailbox",
		}
	}
	fromMailbox := fromMailboxes[0]
	if _, signsFor := mailfauna.SelectSigningDomain(fromMailbox.Host, s.backend.cfg.current().localDomains); signsFor {
		if err := s.assertSenderOwned(fromMailbox.Mailbox, fromMailbox.Host, authedLocal, actorID); err != nil {
			if s.backend.logger != nil {
				s.backend.logger.Info("mta: submission rejected — From: header not owned by the authenticated actor",
					"from", fromMailbox.Mailbox+"@"+fromMailbox.Host,
					"source_ip", s.sourceIP,
				)
			}
			return err
		}
	}

	// Strip internal `Received:` headers the user's MUA / upstream relays
	// attached, so the submitter's IP and our internal hostnames don't reach
	// the recipient (smtp-server.md § Outbound delivery). It runs here, before
	// the enqueue, so the signature the nest adds at the outbound hand-out
	// covers the stripped form. The strip is the shared-Rust pure fn over UniFFI —
	// case-insensitive, RFC 5322 §2.2.3 continuation-aware, substring-safe.
	raw = mailfauna.StripReceivedHeaders(raw)

	// The reserved `X-Fauna-*` delivery stamps go the same way, for the same
	// reason and in the same place — before the enqueue, so the signature covers
	// the stripped form: the Fauna-recipient copies, the Sent copy and the
	// relayed message are all filed from these bytes, and the stamps are
	// written only by the door that files a copy (smtp-server.md
	// § Architectural rules → The X-Fauna-* namespace). `X-Fauna-Forwarded-By`
	// survives (mail-forwarding.md § Loop detection).
	raw = mailfauna.StripFaunaHeaders(raw)

	// RFC 6409 §8.3: a submission server SHOULD add a Message-ID when the
	// MUA omitted one. Not cosmetic here: the nest's enqueue_outbound_mail
	// rejects an EMPTY original_msgid, so a Message-ID-less submission used
	// to die at DATA with a misleading transient 451 ("Outbound enqueue
	// temporarily unavailable") that no retry can ever clear — root-caused
	// live 2026-07-09 against example.com + the :latest image. Stamped BEFORE
	// the enqueue so the DKIM signature covers it and the Sent copy carries it.
	if extractMessageID(raw) == "" {
		idDomain := s.backend.cfg.current().primaryDomain
		if idDomain == "" {
			idDomain = "unconfigured.invalid"
		}
		raw = prependHeaders(raw, []string{
			fmt.Sprintf("Message-ID: <%s@%s>", generateMessageIDToken(), idDomain),
		})
	}

	if notLocal := s.checkFromLocal(raw); notLocal != nil {
		// From: header domain is not one of our local domains — reject
		// permanently per mail-multidomain.md § Signing-key selection at
		// outbound time (RFC 6376 §3.6 d= must be a domain we host).
		if s.backend.logger != nil {
			s.backend.logger.Info("mta: submission rejected — From: domain not local",
				"from_domain", notLocal.domain)
		}
		return &gosmtp.SMTPError{
			Code:         550,
			EnhancedCode: gosmtp.EnhancedCode{5, 7, 7},
			Message:      "From: domain not local",
		}
	}

	msgID := extractMessageID(raw)

	// No nest client wired — the listener has been spun up purely for
	// test fixtures that exercise the SMTP wire shape without a real
	// nest. Refuse with a clean 451 so the test sees a deterministic
	// error rather than a silent success.
	if s.backend.client == nil {
		return &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 3, 0},
			Message:      "Outbound delivery unavailable (no nest client wired)",
		}
	}

	// Parse the body so the index hint can be tokenized from
	// Subject + body text (the same shape the C.9 inbound path uses).
	// A parse failure here is internal — the bytes already passed the
	// From: gates above; tempfail rather than bounce.
	parsed, err := mailfauna.ParseRFC5322(raw)
	if err != nil {
		if s.backend.logger != nil {
			s.backend.logger.Warn("mta: parse submitted body failed", "err", err)
		}
		return &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 5, 0},
			Message:      "Submission processing failed; try again later",
		}
	}

	// Partition recipients into Fauna vs external for the two delivery
	// paths. Local recipients were already resolved at RCPT TO (see
	// Rcpt → resolveLocalRecipient); Data consumes that cache rather
	// than re-calling resolve_recipient. External recipients go
	// through enqueue_outbound_mail. The sender's "Sent" copy is always
	// submitted via submit_inbound_mail (the dispatch helper handles it).
	cfg := s.backend.cfg.current()
	_, externalRcpts := partitionRecipientsByLocalDomains(rcpts, cfg.localDomains)

	// External-recipient over-inline-budget body → stage on the bulk-byte plane
	// under a one-shot AEAD envelope (smtp-server.md § Message size limits, the
	// staged-envelope rule): the perimeter admits raw messages far larger than
	// the 2 MiB WS-RPC frame, and `enqueue_outbound_mail` ships the body
	// inline, so over the inline budget the body must ride by reference instead.
	// Staged HERE, before any local copy is delivered: a staging failure must
	// fail the whole DATA (one reply covers every recipient — a delivered local
	// copy would duplicate on the sender's resend). The PRODUCT-ceiling 552
	// already fired at the DATA-read max_message_bytes clamp above; this leg only
	// decides inline-vs-staged transport for a body the perimeter admitted.
	var stagedRef *wsrpc.StagedBodyRef
	if len(externalRcpts) > 0 && mailfauna.MailBodyNeedsReference(uint64(len(raw)), 0) {
		stageCtx, stageCancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
		ref, stageErr := stageOutboundEnvelope(stageCtx, s.backend.client, s.backend.bytePlane, s.actorID, raw)
		stageCancel()
		if stageErr != nil {
			// Staging failure is transient (byte plane unwired / upload error) —
			// the message is fine, our reach to nest is not. A 451 lets the MUA
			// retry once we are wired; nothing was delivered yet.
			if s.backend.logger != nil {
				s.backend.logger.Warn("mta: stage outbound body failed (→451)", "err", stageErr)
			}
			return &gosmtp.SMTPError{
				Code:         451,
				EnhancedCode: gosmtp.EnhancedCode{4, 5, 0},
				Message:      "Outbound staging temporarily unavailable; try again later",
			}
		}
		stagedRef = ref
	}

	// Split the RCPT-time-resolved local recipients into normal local mailboxes
	// (sealed local copy) and admin external forwarders (mail-aliases.md § Kind 7
	// — redirect to an external target, no local copy). A local user emailing a
	// forwarder address (e.g. info@<our-domain>) reaches the external destination
	// through the same shared forward dispatch the inbound MX path uses.
	var forwarders []resolvedLocalRcpt
	localMailboxes := make([]resolvedLocalRcpt, 0, len(localResolved))
	for _, r := range localResolved {
		if r.forward {
			forwarders = append(forwarders, r)
			continue
		}
		localMailboxes = append(localMailboxes, r)
	}
	faunaActors := dedupLocalRecipients(localMailboxes, s.actorID)

	// senderDomain = primary_domain anchor — used for the Received:
	// header's by= clause + the dispatch helper's audit logging.
	senderDomain := cfg.primaryDomain
	// The submitter's envelope address rides to the nest's ingest so the guardian
	// mail gate can recompute a `hold` for a supervised recipient.
	s.mu.Lock()
	envelopeFrom := s.envelopeFrom
	s.mu.Unlock()
	// Admin external-forwarder redirects (mail-aliases.md § Kind 7): redirect to
	// the external target, attributed to the managing admin (copy_mode=redirect,
	// no local copy). Dispatched FIRST, before any local copy or the Sent copy:
	// a forward nest did not take answers 451 (the message would otherwise exist
	// nowhere for that recipient — mail-forwarding.md § Queue ceiling), and
	// DATA's one reply covers every recipient, so nothing this envelope's resend
	// would duplicate may have committed yet.
	for _, fwd := range forwarders {
		if tempErr := s.dispatchForwarderRedirect(&parsed, raw, sender, fwd); tempErr != nil {
			return tempErr
		}
	}

	if err := s.dispatchFaunaRecipients(raw, &parsed, faunaActors, senderDomain, envelopeFrom); err != nil {
		return err
	}

	if len(externalRcpts) > 0 {
		ctx, cancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
		defer cancel()
		// nil onBehalfOfActor: the MTA submission path is sender-unconstrained
		// (nest trusts the AUTH'd submitter); only the MDA auto-schedule gateway
		// passes a caller-scope actor. When the body was staged above, the inline
		// raw is empty and the reference carries it (mutually exclusive: nest
		// recovers the identical bytes from the staged ciphertext).
		outboundRaw := raw
		if stagedRef != nil {
			outboundRaw = nil
		}
		if _, err := wsrpc.EnqueueOutboundMail(ctx, s.backend.client, msgID, sender, externalRcpts, outboundRaw, nil, stagedRef); err != nil {
			// Surface the decoded RpcError (mirrors dispatchFaunaRecipients'
			// deliverOne site): the opaque "ok=false (payload N bytes)" form
			// cost a multi-hour live root-cause 2026-07-09 — the payload
			// carried the answer ("original_msgid must not be empty") the
			// whole time.
			if s.backend.logger != nil {
				if code, detail, ok := wsrpc.RpcErrorDetail(err); ok {
					s.backend.logger.Warn("mta: enqueue_outbound_mail failed (→451)",
						"err", err, "rpc_code", code, "rpc_details", detail)
				} else {
					s.backend.logger.Warn("mta: enqueue_outbound_mail failed", "err", err)
				}
			}
			return &gosmtp.SMTPError{
				Code:         451,
				EnhancedCode: gosmtp.EnhancedCode{4, 5, 0},
				Message:      "Outbound enqueue temporarily unavailable; try again later",
			}
		}
		if s.backend.outboundTrigger != nil {
			s.backend.outboundTrigger()
		}
	}

	if s.backend.logger != nil {
		s.backend.logger.Info("mta: submission delivered",
			"sender", sender,
			"fauna_actors", len(faunaActors),
			"forwarders", len(forwarders),
			"external_rcpts", len(externalRcpts),
			"message_id", msgID,
			"size_bytes", len(raw),
		)
	}
	return nil
}

// authedFromAddress reconstructs the canonical `<handle>@<primary-domain>`
// address the session authenticated as. It is the Data() fallback used
// only when no MAIL FROM was validated first (a direct-Data fixture);
// the normal outbound envelope sender is the validated envelopeFrom.
// Caller MUST hold s.mu.
func (s *submissionSession) authedFromAddress() string {
	primaryDomain := s.backend.cfg.current().primaryDomain
	if s.authedLocalPart == "" || primaryDomain == "" {
		return ""
	}
	return s.authedLocalPart + "@" + primaryDomain
}

// extractMessageID scans the message header block for the Message-ID:
// header and returns its bracket-stripped value. Returns an empty
// string when the header is absent — nest tolerates an empty
// `original_msgid` because bounce-history is keyed on
// (sender, msgid) and an empty msgid simply means no NDR
// rate-limiting fires for this submission. A no-op on a malformed
// header block (no headers / no blank-line terminator).
// generateMessageIDToken returns the local part for a
// submission-server-generated Message-ID (RFC 6409 §8.3): the shared
// self-describing Fauna mint (96 random bits + a 32-bit provenance tag) via
// the UniFFI binding. The tag is load-bearing — the nest's guardian mail
// gate seeds its DSN correlation only for ids it can *verify* a Fauna path
// minted, so the old plain-random token (right shape, no tag) would have
// silently stopped seeding and held every bounce of submission-stamped mail
// (`family-safety.md` § The mail gate). The old timestamp fallback is gone
// with it: the shared mint's randomness failure is a panic, not a weak id.
func generateMessageIDToken() string {
	return mailfauna.NewFaunaMsgidLocal()
}

func extractMessageID(raw []byte) string {
	// Find the end of headers (CRLF CRLF or LF LF).
	end := len(raw)
	if i := bytesIndex(raw, []byte("\r\n\r\n")); i >= 0 {
		end = i
	} else if i := bytesIndex(raw, []byte("\n\n")); i >= 0 {
		end = i
	}
	headers := raw[:end]
	// Walk header lines. Folded lines start with whitespace; we don't
	// need to unfold for Message-ID (it's never folded in practice).
	start := 0
	for start < len(headers) {
		// Find the next line terminator.
		eol := start
		for eol < len(headers) && headers[eol] != '\n' {
			eol++
		}
		line := headers[start:eol]
		// Strip trailing \r if present.
		if n := len(line); n > 0 && line[n-1] == '\r' {
			line = line[:n-1]
		}
		// Case-insensitive prefix match.
		const want = "Message-ID:"
		if len(line) >= len(want) && strings.EqualFold(string(line[:len(want)]), want) {
			v := strings.TrimSpace(string(line[len(want):]))
			v = strings.TrimPrefix(v, "<")
			v = strings.TrimSuffix(v, ">")
			return v
		}
		start = eol + 1
	}
	return ""
}

// bytesIndex is a tiny stdlib-free analogue of bytes.Index for the
// extractMessageID needle scan. Avoids a dep on the bytes package in
// the submission hot-path (one-off scan per Data).
func bytesIndex(haystack, needle []byte) int {
	if len(needle) == 0 || len(needle) > len(haystack) {
		return -1
	}
	for i := 0; i <= len(haystack)-len(needle); i++ {
		match := true
		for j := 0; j < len(needle); j++ {
			if haystack[i+j] != needle[j] {
				match = false
				break
			}
		}
		if match {
			return i
		}
	}
	return -1
}

// errFromNotLocal signals that the message's From: header domain is not one
// of the deployment's local domains (and not a subdomain of any) — Data() maps
// it to `550 5.7.7 From: domain not local` per mail-multidomain.md
// § Signing-key selection at outbound time.
type errFromNotLocal struct{ domain string }

func (e errFromNotLocal) Error() string {
	return fmt.Sprintf("From: domain %q not local", e.domain)
}

// checkFromLocal is the From: header locality gate: the nest signs an outbound
// message under its From: header domain (RFC 6376 §3.6), so a deployment that
// signs refuses a From: on a domain it does not host. It returns nil when
//   - the deployment signs for no domain (the config snapshot projects no
//     `dkim_selectors` — the localhost / pre-claim steady state), so there is
//     no signing domain to hold the header to;
//   - the From header can't be parsed — locality can't be decided; or
//   - the From domain is an active local domain or a subdomain of one
//     (mailfauna.SelectSigningDomain, the shared rule the nest's signer keys
//     on — never re-implemented in Go);
//
// and errFromNotLocal otherwise — Data() 550's. Both inputs come from the
// hot-reloaded config snapshot; the bridge holds no DKIM key and signs nothing.
func (s *submissionSession) checkFromLocal(raw []byte) *errFromNotLocal {
	cfg := s.backend.cfg.current()
	if !cfg.signsOutbound {
		return nil
	}
	fromDomain := submissionFromDomain(raw)
	if fromDomain == "" {
		if s.backend.logger != nil {
			s.backend.logger.Info("mta: submission From-locality check skipped — no parseable From header")
		}
		return nil
	}
	if _, local := mailfauna.SelectSigningDomain(fromDomain, cfg.localDomains); !local {
		return &errFromNotLocal{domain: fromDomain}
	}
	return nil
}

// submissionFromDomain extracts the lowercased domain of the message's From:
// header (the d= anchor per RFC 6376 §3.6), or "" when the message has no
// parseable From header. The envelope MAIL FROM is deliberately NOT consulted —
// a DKIM signature is on behalf of the From: header domain, not the bounce
// address.
func submissionFromDomain(raw []byte) string {
	return mailfauna.SenderDomainWithEnvelopeFallback(raw, "")
}

// Reset clears the per-transaction state set by D.3's Mail/Rcpt
// gates. The authenticated flag and the unwrapped submission token
// survive a Reset — RSET clears the envelope, not the AUTH state
// (RFC 5321 §4.1.1.5).
func (s *submissionSession) Reset() {
	s.mu.Lock()
	s.envelopeFrom = ""
	s.recipientCount = 0
	s.recipients = nil
	s.localResolved = nil
	s.mu.Unlock()
}

// Logout signals connection close. No mlock'd secrets are held at the
// D.1 layer; D.2's unwrapped submission capability lifetime ends here.
func (s *submissionSession) Logout() error {
	if s.backend != nil && s.backend.drain != nil {
		s.loggedOut.Do(s.backend.drain.leave)
	}
	return nil
}

// ── listener wiring ──────────────────────────────────────────────

// submissionListenerKind disambiguates the two production submission
// listeners. The two share one backend; the difference is only how
// TLS is negotiated.
type submissionListenerKind int

const (
	// submissionImplicitTLS is the port-465 listener — the connection
	// is TLS-handshaken at accept (tls.NewListener wraps the raw
	// net.Listener).
	submissionImplicitTLS submissionListenerKind = iota
	// submissionStartTLS is the port-587 listener — the connection
	// starts plain and gosmtp speaks STARTTLS in-protocol. With
	// `AllowInsecureAuth = false` and the gating in submissionSession,
	// AUTH (and thus MAIL FROM) is unavailable before STARTTLS.
	submissionStartTLS
)

// runSubmissionListener serves submissions on l with the given
// backend until ctx is cancelled. Exposed for tests so they can
// inject self-signed TLS configs without going through tls.Provider.
//
// `l` MUST already be TLS-wrapped when `kind == submissionImplicitTLS`
// (the caller is responsible for the wrap so tests can swap in a
// fixture cert). `kind == submissionStartTLS` takes a plain listener;
// the function configures srv.TLSConfig so gosmtp negotiates STARTTLS
// on the wire.
//
// Returns nil on clean shutdown (ctx.Done()) or an error on a Serve
// failure that isn't an expected close.
func runSubmissionListener(
	ctx context.Context,
	l net.Listener,
	backend gosmtp.Backend,
	kind submissionListenerKind,
	tlsConfig *tls.Config,
	domain string,
	maxMessageBytes uint32,
	grace time.Duration,
	drain *drainTracker,
	logger *slog.Logger,
) error {
	srv := gosmtp.NewServer(backend)
	srv.Domain = domain
	srv.MaxRecipients = 100
	// EHLO `SIZE` — the effective ceiling, same rule and same boot-snapshot
	// rationale as the inbound listener (server.go § runListenerWithBackend).
	// A submitting MUA is exactly the client that benefits from an honest SIZE:
	// it can refuse an over-large attachment locally instead of uploading it.
	srv.MaxMessageBytes = int64(mailfauna.EffectiveMaxRawMessageBytes(maxMessageBytes))
	srv.ReadTimeout = 5 * time.Minute
	srv.WriteTimeout = 5 * time.Minute
	srv.AllowInsecureAuth = false

	switch kind {
	case submissionImplicitTLS:
		// The listener is already TLS-wrapped; gosmtp still needs the
		// TLSConfig for protocol-level features (e.g., handling
		// renegotiation or post-STARTTLS state — even on implicit
		// TLS, the server may consult it for various paths).
		srv.TLSConfig = tlsConfig
	case submissionStartTLS:
		if tlsConfig == nil {
			return errors.New("submissionStartTLS requires non-nil tlsConfig")
		}
		srv.TLSConfig = tlsConfig
	default:
		return fmt.Errorf("unknown submission listener kind %d", kind)
	}

	// The connection cap is applied to the raw listener at construction (the
	// submission listeners are wrapped in connlimit.New in mta.Run, below
	// tls.NewListener for the implicit-TLS port so go-smtp still sees a
	// *tls.Conn). `l` here is therefore already cap-wrapped; serving and
	// draining it is unchanged.
	logger.Info("submission listener serving",
		"addr", l.Addr().String(),
		"domain", domain,
		"kind", submissionListenerKindString(kind),
	)

	serveErrCh := make(chan error, 1)
	go func() {
		serveErrCh <- srv.Serve(l)
	}()

	select {
	case <-ctx.Done():
		// Graceful drain shared with the inbound listener (T2.6).
		if gracefulDrain(srv, l, drain, grace, serveErrCh, logger) {
			return bridgeshutdown.ErrShutdownForced
		}
		return nil
	case err := <-serveErrCh:
		if err != nil && !errors.Is(err, net.ErrClosed) && !errors.Is(err, gosmtp.ErrServerClosed) {
			return fmt.Errorf("submission serve: %w", err)
		}
		return nil
	}
}

// submissionListenerKindString is the logger attribute for the
// listener kind; lowercase + short so it groups cleanly under a
// `kind=<value>` slog label.
func submissionListenerKindString(k submissionListenerKind) string {
	switch k {
	case submissionImplicitTLS:
		return "implicit-tls"
	case submissionStartTLS:
		return "starttls"
	default:
		return "unknown"
	}
}
