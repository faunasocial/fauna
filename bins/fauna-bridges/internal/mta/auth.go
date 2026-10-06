// Phase D.2: AEAD-unwrap-as-auth flow for SMTP submission.
//
// Mirrors `internal/mda/imap/auth.go` in structure (parse mech payload
// → resolve actor → fetch wrapped blob → AEAD-unwrap → stash on session
// → report_auth_event) but consumes the *submission-token* path: a
// separate blob shape from the IMAP MSEK blob, AEAD-sealed under the
// MUA's credential, plaintext is a signed `SubmissionToken` policy
// struct. See:
//
//   - the mail-bridge rearchitecture design (tracked internally)
//     § Outbound submission (steps 1–2 are this file's contract).
//   - the wrapped-blob crypto design (tracked internally)
//     § Per-credential submission-token blob — the AEAD/signature
//     envelope.
//
// **Substitution-resistance.** The FFI's
// `UnsealSubmissionTokenBlob` rebuilds the AAD from the
// bridge-resolved (actor_id, credential_id), reconstructs the user's
// Ed25519 verifying key from `actor_id` (codebase invariant: actor_id
// IS the 32-byte Ed25519 verifying-key, per
// `bins/fauna-nest/src/registration.rs:184`), and verifies the inner
// `SubmissionToken.user_sig` against it. A coerced/compromised nest
// cannot fabricate submission tokens because forging the inner
// signature requires the user's primary signing key.
//
// **Exactly-one ReportAuthEvent contract.** Every code path that
// reaches `resolveActor` fires exactly one `report_auth_event` (success
// or fail) before the wire reply. Tests pin this with a counting fake.
package mta

import (
	"context"
	"errors"
	"fmt"

	"github.com/emersion/go-sasl"
	gosmtp "github.com/emersion/go-smtp"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/auth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// The credential_id is resolved per AUTH attempt from the submission username's
// RFC 5233 "+suffix" (auth.CredentialFromLocalPart) — `<handle>+<credential_id>@…`,
// defaulting to auth.DefaultCredentialID for a bare username. See
// `docs/goal/behavior/mail-credentials.md` § MUA-username convention.

// errSubmissionAuthFailed is the opaque SMTP wire reply for any
// AUTH failure (RFC 4954 § 5.4: `535 5.7.8 Authentication credentials
// invalid`). The wording is deliberately generic to avoid leaking
// which step failed; the audit-log entry carries the redacted reason.
var errSubmissionAuthFailed = &gosmtp.SMTPError{
	Code:         535,
	EnhancedCode: gosmtp.EnhancedCode{5, 7, 8},
	Message:      "Authentication credentials invalid",
}

// errSubmissionAuthInternal is used when nest is unreachable or
// returns an unexpected shape (transport / decode failure mid-AUTH).
// SMTP `454 4.7.0 Temporary authentication failure` per RFC 4954 §
// 6 — distinguishes "wrong credential" (535) from "try again later".
var errSubmissionAuthInternal = &gosmtp.SMTPError{
	Code:         454,
	EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
	Message:      "Temporary authentication failure",
}

// errSubmissionAuthLockedOut is the D.7 brute-force-protection
// wire response. SMTP `421 4.7.0` ends the session (RFC 5321 §
// 4.3.2: "service not available, closing transmission channel")
// rather than allowing the client to retry within the same TCP
// connection. The wording is deliberately the same as the
// transient-internal-failure 4xx so the wire shape doesn't
// distinguish lockout-versus-other-temp-fail to an attacker. The
// audit-log entry (skipped by design — see runAuth's lockout
// branch) carries no signal that nest can use to coordinate cross-
// bridge throttling either; the gate is local-only.
var errSubmissionAuthLockedOut = &gosmtp.SMTPError{
	Code:         421,
	EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
	Message:      "Too many authentication failures; try again later",
}

// authenticate dispatches a single AUTH attempt to the per-mech
// workhorse. Mirrors `internal/mda/imap/auth.go::Session.authenticate`
// down to the dispatch shape; the per-mech bodies differ only in how
// they parse the payload.
func (s *submissionSession) authenticate(mech, payload string) error {
	switch mech {
	case sasl.Plain:
		return s.authenticatePLAIN(payload)
	case sasl.OAuthBearer:
		return s.authenticateOAUTHBEARER(payload)
	default:
		// Unreachable on the production path — Auth() rejects
		// unknown mechs before reaching authenticate(). Kept as a
		// defense-in-depth no-credential-leak guard.
		return fmt.Errorf("mta: SASL mechanism %q not supported", mech)
	}
}

func (s *submissionSession) authenticatePLAIN(payload string) error {
	username, password, err := auth.ParsePlainPayload(payload)
	if err != nil {
		// Pre-resolution failure: no actor_id yet. The fail-report
		// uses zero actor_id (mirrors IMAP's same-shape branch).
		return s.reportAuthFail(nil, auth.DefaultCredentialID, err)
	}
	return s.runAuth(username, []byte(password), mailfauna.KdfKindArgon2id)
}

func (s *submissionSession) authenticateOAUTHBEARER(payload string) error {
	username, token, ok := auth.ParseOAuthBearerPayload(payload)
	if !ok {
		return s.reportAuthFail(nil, auth.DefaultCredentialID, errors.New("OAUTHBEARER payload malformed"))
	}
	return s.runAuth(username, []byte(token), mailfauna.KdfKindHkdf)
}

// runAuth is the post-parse workhorse shared by both mechs:
//
//  0. D.7 brute-force gate: if the (username, credential_id, source_ip)
//     bucket has hit the per-1-minute failure ceiling, return SMTP
//     `421 4.7.0` immediately — no validate_recipient call, no
//     fetch_wrapped_submission_token call, no AEAD-unwrap. The
//     attacker gets no timing oracle distinguishing valid from
//     invalid credentials.
//  1. SplitEmail(user) — bridge identity check (local + domain).
//  2. validate_recipient(local, domain) → actor_id (32B Ed25519 vkey).
//  3. fetch_wrapped_submission_token(actor_id, credential_id) → blob.
//  4. UnsealSubmissionTokenBlob(blob, secret, actor_id, credential_id, kind)
//     → SubmissionToken plaintext. AEAD-success + inner-sig-success
//     is the authentication-success signal.
//  5. Stash actor_id, credential_id, SubmissionToken, authenticated=true.
//     Reset the lockout bucket for this key (a legitimate user
//     shouldn't be punished by their prior typo-fests).
//  6. report_auth_event(result=ok).
//
// Every code path that reaches step 2 fires exactly one report
// (success at step 6 or fail at any earlier branch via
// `reportAuthFail`). The D.7 lockout branch is the *only* exit that
// skips report_auth_event — the audit log doesn't see the lockout
// 421 (the bridge surfaces lockout events via the bridge-local
// "AUTH locked out" log line instead). Skipping the audit avoids
// inflating audit storage during a sustained brute-force.
func (s *submissionSession) runAuth(username string, secret []byte, kind mailfauna.KdfKind) error {
	// A bare username (no `@domain`) resolves under the box's PrimaryDomain —
	// uniform with the CalDAV + IMAP AUTH surfaces (auth.SplitEmailDefault), so
	// a client that submits with just the local part authenticates the same way
	// everywhere. Empty PrimaryDomain ⇒ strict `user@domain`.
	local, domain, ok := auth.SplitEmailDefault(username, s.backend.cfg.current().primaryDomain)
	if !ok {
		return s.reportAuthFail(nil, auth.DefaultCredentialID, errors.New("malformed user@domain"))
	}
	// RFC 5233 sub-addressing: `<base>+<credential_id>@<domain>`. The base
	// resolves the actor; the suffix selects the wrapped submission-token blob.
	// A bare username maps to DefaultCredentialID (mail-credentials.md
	// § MUA-username).
	base, credentialID := auth.CredentialFromLocalPart(local)
	// The lockout keys on the canonical (base, domain) principal — NOT the raw
	// presented username — so every form that resolves to this credential
	// (`alice`, `alice+`, `alice+default`, `alice@<primary>`) shares one bucket
	// (auth.PrincipalKey; § B7). The raw username is still logged for forensics.
	principal := auth.PrincipalKey(base, domain)
	// Snapshot the per-credential lockout once at the request boundary; a
	// `config_changed` rebuild swaps the holder's bundle (and its AuthLockout),
	// but this AUTH attempt uses the instance live at its start.
	lockout := s.backend.cfg.current().authLockout
	if lockout != nil && lockout.IsLockedFor(principal, credentialID, s.sourceIP) {
		if s.backend.logger != nil {
			s.backend.logger.Warn("mta: AUTH locked out",
				"username", username,
				"credential_id", credentialID,
				"source_ip", s.sourceIP,
			)
		}
		return errSubmissionAuthLockedOut
	}

	ctx, cancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
	defer cancel()

	actorID, err := s.resolveActor(ctx, base, domain)
	if err != nil {
		// resolveActor returns ErrRecipientRejected when nest rejects;
		// other errors are transport/decode. Map both to AUTH-fail on
		// the wire (no per-error wire distinction — attackers could
		// otherwise probe valid-vs-invalid recipients).
		lockout.RecordFailureFor(principal, credentialID, s.sourceIP)
		return s.reportAuthFail(nil, credentialID, err)
	}

	blob, err := wsrpc.FetchWrappedSubmissionToken(ctx, s.backend.client, actorID, credentialID)
	if err != nil {
		lockout.RecordFailureFor(principal, credentialID, s.sourceIP)
		return s.reportAuthFail(actorID, credentialID, err)
	}
	if blob == nil {
		// Nest has no submission token on file for this (actor,
		// credential). User hasn't enabled SMTP submission via their
		// Fauna app yet. Wire response is the same opaque 535.
		lockout.RecordFailureFor(principal, credentialID, s.sourceIP)
		return s.reportAuthFail(actorID, credentialID, errors.New("no submission token on file"))
	}

	token, err := mailfauna.UnsealSubmissionTokenBlob(blob, secret, actorID, credentialID, kind)
	if err != nil {
		// AEAD-fail or inner-signature-fail. The mailfauna error
		// wording carries the discriminant ("AEAD verify failed" vs
		// "signature verify failed"); both feed RedactedFailReason
		// into the audit log.
		lockout.RecordFailureFor(principal, credentialID, s.sourceIP)
		return s.reportAuthFail(actorID, credentialID, err)
	}

	s.mu.Lock()
	s.actorID = actorID
	s.credentialID = credentialID
	s.authedLocalPart = base
	s.submissionToken = token
	s.authenticated = true
	s.mu.Unlock()

	lockout.ResetFor(principal, credentialID, s.sourceIP)

	occurredAt := uint64(s.now().UnixMilli())
	if err := wsrpc.ReportAuthEvent(
		ctx, s.backend.client, actorID, credentialID, "ok", s.sourceIP, "", occurredAt,
	); err != nil {
		// Audit-log failure on the success path doesn't roll back
		// the AUTH (the user successfully unwrapped) — but log it so
		// admins see audit gaps.
		if s.backend.logger != nil {
			s.backend.logger.Warn("mta: report_auth_event(ok) failed", "err", err)
		}
	}
	return nil
}

// reportAuthFail fires a result=fail audit event and returns the
// SMTP 535 5.7.8 to the SASL caller. Always returns
// errSubmissionAuthFailed (never errSubmissionAuthInternal — the
// MUA gets one unambiguous failure code regardless of cause, the
// audit-log differentiates by reason).
//
// `actorID == nil` means "we never resolved an actor"; nest requires
// a 32-byte actor_id, so a zeros-padded value goes onto the audit
// row (mirrors the IMAP MDA path's same branch).
func (s *submissionSession) reportAuthFail(actorID []byte, credentialID string, err error) error {
	ctx, cancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
	defer cancel()
	occurredAt := uint64(s.now().UnixMilli())
	auditActor := actorID
	if auditActor == nil {
		auditActor = make([]byte, 32)
	}
	if rerr := wsrpc.ReportAuthEvent(
		ctx, s.backend.client, auditActor, credentialID, "fail", s.sourceIP, auth.ReasonRedacted, occurredAt,
	); rerr != nil {
		if s.backend.logger != nil {
			s.backend.logger.Warn("mta: report_auth_event(fail) failed", "report_err", rerr, "auth_err", err)
		}
	}
	if s.backend.logger != nil && err != nil {
		s.backend.logger.Info("mta: AUTH failed", "reason", auth.RedactedFailReason(err), "source_ip", s.sourceIP)
	}
	return errSubmissionAuthFailed
}

// resolveActor calls fauna.bridges.validate_recipient and returns the
// 32-byte actor_id on success. Mirrors the IMAP path's same-named
// helper; the wsrpc wrapper already lifts hex into bytes for us.
func (s *submissionSession) resolveActor(ctx context.Context, localPart, domain string) ([]byte, error) {
	raw, _, err := wsrpc.ValidateRecipient(ctx, s.backend.client, localPart, domain)
	if err != nil {
		return nil, err
	}
	if len(raw) != 32 {
		return nil, fmt.Errorf("validate_recipient actor_id must be 32 bytes, got %d", len(raw))
	}
	return raw, nil
}
