package imap

import (
	"context"
	"errors"
	"fmt"

	"github.com/emersion/go-sasl"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/auth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// errAuthFailed is the standard error returned to the SASL server on
// any AUTH failure (PLAIN / OAUTHBEARER alike). emersion/go-imap
// translates a returned error into `<tag> NO ...`; the wording here
// is deliberately generic to avoid leaking which step failed.
var errAuthFailed = errors.New("imap: authentication failed")

// The credential_id is resolved per AUTH attempt from the MUA username's RFC
// 5233 "+suffix" (auth.CredentialFromLocalPart) — `<handle>+<credential_id>@…`,
// defaulting to auth.DefaultCredentialID for a bare username. See
// `docs/goal/behavior/mail-credentials.md` § MUA-username convention.

// ── SessionSASL ───────────────────────────────────────────────────

// AuthenticateMechanisms reports the SASL mechanisms this Session
// supports. emersion/go-imap consults this from the post-TLS
// CAPABILITY response and dispatches the wire AUTHENTICATE verb to
// `Authenticate(mech)`.
//
// Per imap-server.md § Authentication: PLAIN and OAUTHBEARER over
// TLS only. STARTTLS-required gating happens server-side via
// `InsecureAuth=false`; this method is only consulted post-TLS.
func (s *Session) AuthenticateMechanisms() []string {
	return []string{sasl.Plain, sasl.OAuthBearer}
}

// Authenticate returns the SASL server emersion/go-imap drives for
// AUTHENTICATE <mech>. The returned server's authenticator closure
// dispatches to `Session.authenticate(mech, payload)` — the
// workhorse that owns the AEAD-unwrap-as-auth contract.
func (s *Session) Authenticate(mech string) (sasl.Server, error) {
	switch mech {
	case sasl.Plain:
		return sasl.NewPlainServer(func(identity, username, password string) error {
			if identity != "" && identity != username {
				// RFC 4616: PLAIN authorization identity equal to the
				// authentication identity OR empty. Anything else is
				// "authorization identity not supported".
				return errAuthFailed
			}
			return s.authenticate(sasl.Plain, auth.BuildPlainPayload(identity, username, password))
		}), nil
	case sasl.OAuthBearer:
		return sasl.NewOAuthBearerServer(func(opts sasl.OAuthBearerOptions) *sasl.OAuthBearerError {
			payload := auth.BuildOAuthBearerPayload(opts.Username, opts.Token)
			if err := s.authenticate(sasl.OAuthBearer, payload); err != nil {
				// RFC 7628 § 3.2.2: invalid_token covers both the
				// "bad token format" and "token rejected" cases.
				return &sasl.OAuthBearerError{
					Status:  "invalid_token",
					Schemes: "bearer",
				}
			}
			return nil
		}), nil
	default:
		return nil, fmt.Errorf("imap: SASL mechanism %q not supported", mech)
	}
}

// ── Workhorse ─────────────────────────────────────────────────────

// authenticate runs the AEAD-unwrap-as-auth flow for a single
// AUTHENTICATE attempt. Same shape for both supported SASL mechs:
//
//  1. Parse the per-mech payload (PLAIN: \x00user\x00pass;
//     OAUTHBEARER: GS2 framing with a=<user>, Bearer <token>).
//  2. validate_recipient(user) → actor_id (hex string).
//  3. fetch_wrapped_mls_blob(actor_id, credential_id) → blob bytes.
//  4. UnwrapMLSBlob(blob, secret, actor_id, credential_id, kind) →
//     mlock'd MLSCapability. AEAD-success is the auth signal.
//  5. fetch_recipient_mls_pubkey(actor_id) → cache the pubkey.
//  6. report_auth_event(result=ok / fail) — every code path that
//     reaches step 2 fires exactly one report (success or fail).
//
// Per imap-server.md § Authentication; the failure paths NEVER log
// or pass the credential bytes to anything except the AEAD unwrap.
func (s *Session) authenticate(mech, payload string) error {
	switch mech {
	case sasl.Plain:
		return s.authenticatePLAIN(payload)
	case sasl.OAuthBearer:
		return s.authenticateOAUTHBEARER(payload)
	default:
		return fmt.Errorf("imap: SASL mechanism %q not supported", mech)
	}
}

func (s *Session) authenticatePLAIN(payload string) error {
	username, password, err := auth.ParsePlainPayload(payload)
	if err != nil {
		return s.reportAuthFail(nil, auth.DefaultCredentialID, errAuthFailed)
	}
	return s.runGuardedAuth(username, []byte(password), mailfauna.KdfKindArgon2id)
}

func (s *Session) authenticateOAUTHBEARER(payload string) error {
	// RFC 7628 § 3.1 client-first message:
	//   gs2-header (n,a=<user>,) \x01 host=... \x01 auth=Bearer <tok> \x01 \x01
	// We only need the user and token; host/port are advisory and
	// already trusted at the TLS layer.
	user, token, ok := auth.ParseOAuthBearerPayload(payload)
	if !ok {
		return s.reportAuthFail(nil, auth.DefaultCredentialID, errAuthFailed)
	}
	// The credential_id rides the username's RFC 5233 "+suffix" for OAUTHBEARER
	// too (runGuardedAuth resolves it), so a per-credential bearer token at
	// `<handle>+<credential_id>@<domain>` unwraps the matching blob.
	return s.runGuardedAuth(user, []byte(token), mailfauna.KdfKindHkdf)
}

// runGuardedAuth runs the resolve → AEAD-unwrap flow behind the
// per-(credential, source-IP) AUTH-failure lockout (security review
// § D4/M1). It mirrors mta.runAuth exactly:
//
//   - check IsLocked *after* the username is syntactically valid (so we
//     have a stable key) but *before* validate_recipient + the Argon2id
//     unwrap — a locked attacker pays neither the nest round-trip, the
//     KDF cost, nor an AEAD-timing oracle;
//   - RecordFailure on every failure exit that reaches the resolve/unwrap;
//   - Reset on success.
//
// The locked branch is the only AUTH exit that skips report_auth_event —
// matching the MTA, it avoids inflating the audit log during a sustained
// brute-force and surfaces the event via a bridge-local warn instead.
// `s.lockout` is nil-safe (a hand-built test Session is never locked).
func (s *Session) runGuardedAuth(username string, secret []byte, kind mailfauna.KdfKind) error {
	local, domain, ok := auth.SplitEmailDefault(username, s.primaryDomain)
	if !ok {
		return s.reportAuthFail(nil, auth.DefaultCredentialID, errAuthFailed)
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

	if s.lockout.IsLockedFor(principal, credentialID, s.sourceIP) {
		if s.logger != nil {
			s.logger.Warn("imap: AUTH locked out",
				"username", username,
				"credential_id", credentialID,
				"source_ip", s.sourceIP,
			)
		}
		return errAuthFailed
	}

	ctx, cancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
	defer cancel()

	actorID, err := s.resolveActor(ctx, base, domain)
	if err != nil {
		// The user does not exist. Run a dummy KDF so this path costs the same
		// Argon2id as an existing-user wrong-password attempt (finishAuth's
		// UnwrapMLSBlob below); otherwise the KDF-vs-no-KDF timing gap
		// enumerates valid accounts (network-exposure.md § Rulings F3).
		mailfauna.DummyCredentialKDF(secret, kind)
		s.lockout.RecordFailureFor(principal, credentialID, s.sourceIP)
		return s.reportAuthFail(nil, credentialID, err)
	}

	if err := s.finishAuth(ctx, actorID, credentialID, base, secret, kind); err != nil {
		// finishAuth already fired report_auth_event(fail) on its way out.
		s.lockout.RecordFailureFor(principal, credentialID, s.sourceIP)
		return err
	}
	s.lockout.ResetFor(principal, credentialID, s.sourceIP)
	return nil
}

// finishAuth handles the steps after actor resolution: fetch the
// wrapped blob, AEAD-unwrap, cache state, fire the audit event.
// Shared by PLAIN and OAUTHBEARER.
func (s *Session) finishAuth(
	ctx context.Context,
	actorID []byte,
	credentialID string,
	localPart string,
	secret []byte,
	kind mailfauna.KdfKind,
) error {
	blob, err := wsrpc.FetchWrappedMLSBlob(ctx, s.client, actorID, credentialID)
	if err != nil {
		return s.reportAuthFail(actorID, credentialID, err)
	}
	if blob == nil {
		// The user exists but this credential_id has no wrapped blob. Run a
		// dummy KDF so a valid-user-but-unknown-credential attempt is timing-
		// uniform with a wrong-password attempt (network-exposure.md § F3).
		mailfauna.DummyCredentialKDF(secret, kind)
		return s.reportAuthFail(actorID, credentialID, errAuthFailed)
	}

	cap, err := mailfauna.UnwrapMLSBlob(blob, secret, actorID, credentialID, kind)
	if err != nil {
		return s.reportAuthFail(actorID, credentialID, err)
	}

	// Session-level pubkey caching, NOT mail-new-ingest: this feeds
	// IMAP-session seal sites (e.g. the spam-model \Junk re-seal) that have
	// no epoch opener, so it must always resolve the standing key even with
	// the content-sealing-epochs write gate forced on (fix).
	pubkey, mlkemEk, _, err := wsrpc.FetchRecipientMLSPubkeyHybrid(ctx, s.client, actorID, false)
	if err != nil {
		// Pubkey fetch failure is non-fatal for AUTH itself — the
		// MSEK is already unwrapped — but we treat it as auth-fail
		// per spec: the bridge must have the pubkey before letting
		// the session into authenticated state. Zeroize the cap on
		// the failure path so the secret doesn't linger.
		cap.Zeroize()
		return s.reportAuthFail(actorID, credentialID, err)
	}

	// Index pubkey is a separate per-actor public key the MDA seals
	// the APPEND search-index hint to. Unlike the MLS pubkey it is
	// allowed to be None: Phase E will land production provisioning
	// of index keys end-to-end; today the call returns nil for every
	// actor and APPEND surfaces the missing-key error at call time.
	// Treat a transport failure the same as the MLS pubkey path —
	// auth-fail — but a None reply is fine (the variable stays nil).
	indexKey, err := wsrpc.FetchRecipientIndexKey(ctx, s.client, actorID)
	if err != nil {
		cap.Zeroize()
		return s.reportAuthFail(actorID, credentialID, err)
	}

	// MLS-snapshot blob: encrypted under MSEK on nest's disk. The
	// MDA AEAD-unwraps it here and parses it ONCE into the
	// per-connection record opener (Phase-3 S2 / the F2 perf fold),
	// so the per-record serve path (`mailfauna.OpenStoredRecord`)
	// doesn't pay the fetch + unseal + snapshot-parse cost on every
	// message. Same nil-tolerant shape as the index pubkey:
	// transport failure = auth-fail (nest is reachable for the
	// wrapped blob but not for this); nil reply = user hasn't
	// provisioned a snapshot from their primary client yet, AUTH
	// still succeeds, a sealed-record FETCH later surfaces the
	// missing-snapshot error. AEAD-unwrap failure means the blob is
	// tampered/wrong-actor — auth-fail; a blob that unwraps but
	// doesn't PARSE as an MlsSnapshotPlaintext is tampered the same
	// way — auth-fail too.
	snapshotBlob, err := wsrpc.FetchMLSSnapshotBlob(ctx, s.client, actorID)
	if err != nil {
		cap.Zeroize()
		return s.reportAuthFail(actorID, credentialID, err)
	}
	var recordOpener *mailfauna.MailRecordOpener
	var indexSession *faunaFfi.FfiMailIndexSession
	if snapshotBlob != nil {
		snapshotPlaintext, err := cap.Decrypt(snapshotBlob)
		if err != nil {
			cap.Zeroize()
			return s.reportAuthFail(actorID, credentialID, err)
		}
		// The content-index session (content-index.md § Where the index is
		// built — the MDA builder leg), resumed from the same plaintext the
		// record opener parses. It performs a rail read here so the returned
		// session appends to the actor's EXISTING segment chain and — the
		// load-bearing half — has its stage-time re-index guard seeded from
		// what is already published, which is what stops a MUA session's
		// whole-mailbox re-presentation from republishing an index the actor
		// already has.
		//
		// **Deliberately non-fatal.** Unlike the opener above, a failure here
		// does not fail AUTH: the goal doc's fallback is to serve `SEARCH` the
		// old way for this session, exactly as the client leg skips a builder
		// launch rather than failing login. A transient rail outage or a
		// snapshot without grace keys all land
		// here and all cost the session its index and nothing else.
		indexSession = s.resumeIndexSession(cap, snapshotPlaintext, actorID)
		// The epoch-aware constructor (content-sealing-epochs design § 4):
		// this IMAP session opener genuinely serves mail-new-ingest reads
		// (FETCH), so it carries the capability's MSEK for on-demand epoch
		// derivation — unlike the CalDAV/CardDAV davauth opener, which stays
		// on the plain NewMailRecordOpener (its content is never
		// epoch-sealed, site-scoping precedent).
		recordOpener, err = mailfauna.NewEpochAwareMailRecordOpener(cap, snapshotPlaintext)
		// The opener keeps the parsed leaf secrets (and the MSEK copy)
		// Rust-side; the Go-side snapshot plaintext is no longer needed
		// either way — wipe it eagerly before handling the error.
		for i := range snapshotPlaintext {
			snapshotPlaintext[i] = 0
		}
		if err != nil {
			cap.Zeroize()
			return s.reportAuthFail(actorID, credentialID, err)
		}
	}

	s.mu.Lock()
	s.actorID = actorID
	s.credentialID = credentialID
	s.authedLocalPart = localPart
	s.mlsUnwrap = cap
	s.actorMLSPubkey = pubkey
	s.actorMlkemEk = mlkemEk
	s.actorIndexKey = indexKey
	s.recordOpener = recordOpener
	s.indexSession = indexSession
	s.mu.Unlock()

	occurredAt := uint64(s.now().UnixMilli())
	if err := wsrpc.ReportAuthEvent(ctx, s.client, actorID, credentialID, "ok", s.sourceIP, "", occurredAt); err != nil {
		// Audit-log failure on the success path doesn't roll back
		// the AUTH — the user successfully unwrapped — but we log
		// it so admins see audit gaps.
		if s.logger != nil {
			s.logger.Warn("imap: report_auth_event(ok) failed", "err", err)
		}
	}
	return nil
}

// reportAuthFail fires a result=fail audit event and returns
// errAuthFailed to the SASL caller. The underlying `err` is logged
// (no credential bytes ever ride along) but never surfaced to the
// MUA — every wire response is the same opaque NO so a probe
// attacker can't distinguish "wrong password" from "wrong user".
// Always returns errAuthFailed.
func (s *Session) reportAuthFail(actorID []byte, credentialID string, err error) error {
	ctx, cancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
	defer cancel()
	occurredAt := uint64(s.now().UnixMilli())
	// actorID == nil means "we never resolved an actor"; nest
	// requires a 32-byte actor_id, so feed it 32 zeros to keep the
	// audit row well-typed. Real fauna actor_ids are never all zero.
	auditActor := actorID
	if auditActor == nil {
		auditActor = make([]byte, 32)
	}
	if rerr := wsrpc.ReportAuthEvent(
		ctx, s.client, auditActor, credentialID, "fail", s.sourceIP, auth.ReasonRedacted, occurredAt,
	); rerr != nil {
		if s.logger != nil {
			s.logger.Warn("imap: report_auth_event(fail) failed", "report_err", rerr, "auth_err", err)
		}
	}
	if s.logger != nil && err != nil {
		// Strip the credential bytes — the underlying mailfauna
		// error already does (it only carries shape/AEAD wording),
		// but defense-in-depth.
		s.logger.Info("imap: AUTH failed", "reason", auth.RedactedFailReason(err), "source_ip", s.sourceIP)
	}
	return errAuthFailed
}

// resolveActor calls fauna.bridges.validate_recipient and returns
// the 32-byte actor_id. The wrapper already lifts the wire hex into
// raw bytes for us; this helper just adds the length check.
func (s *Session) resolveActor(ctx context.Context, localPart, domain string) ([]byte, error) {
	raw, _, err := wsrpc.ValidateRecipient(ctx, s.client, localPart, domain)
	if err != nil {
		return nil, err
	}
	if len(raw) != 32 {
		return nil, fmt.Errorf("validate_recipient actor_id must be 32 bytes, got %d", len(raw))
	}
	return raw, nil
}

// ── Payload parsers ───────────────────────────────────────────────
//
// SASL payload parsers + the email splitter + the audit-redaction
// helper live in `internal/auth/` so the SMTP submission MTA (Phase
// D.2) can consume the same primitives without duplicating logic.
// See `bins/fauna-bridges/internal/auth/parsers.go`.
