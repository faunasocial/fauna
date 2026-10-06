// Fauna-recipient delivery from the submission listener.
//
// Local-domain recipients are resolved at RCPT TO (see
// submission.go::Rcpt → resolveLocalRecipient), which resolves each
// via `fauna.bridges.resolve_recipient` (the fixed-order alias resolver)
// and confirms an MLS pubkey is on file — one invalid recipient rejects
// per-RCPT (550) without failing the others (smtp-server.md § Recipient
// handling on submission). Data consumes that resolution cache: for each resolved
// recipient it HPKE-seals the submitted body + tokenized index hint
// to the recipient's MLS + index pubkeys and hands the envelope to
// `submit_inbound_mail` (when the resolved actor equals the
// authenticated sender) or `ingest_inbound_mail` (when it doesn't).
// External-domain RCPTs collect into a separate slice that Data hands
// to `enqueue_outbound_mail` in one batch (permanent failures there
// surface as an NDR — the only delayed-bounce path for a submission).
//
// A "Sent" copy for the sender's own actor is always submitted via
// `submit_inbound_mail`, irrespective of whether the sender listed
// themselves in TO/CC — this is the local-MTA equivalent of an MUA
// stashing its own outgoing copy. The sender's actor is removed from
// the Fauna-RCPT set before dispatch (dedupLocalRecipients) so the
// Sent copy fires exactly once even when the sender includes
// themselves as a recipient.
//
// The nest routing handler maps the two methods to different
// mailboxes server-side: `submit_inbound_mail` → Sent (\Seen);
// `ingest_inbound_mail` → INBOX/Junk per spam disposition. The
// `is_own_submission` flag is set server-side by which handler runs,
// so a misbehaving bridge cannot smuggle a Fauna-to-Fauna delivery
// into the recipient's Sent folder by claiming "own submission" on
// the wire.
//
// The only failures left at DATA are genuinely transient (seal error,
// ingest/submit transport error); those tempfail the whole DATA (451)
// and the MUA retries.
package mta

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"log/slog"

	gosmtp "github.com/emersion/go-smtp"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/auth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailstage"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"
)

// faunaSubmissionVerdicts is the synthetic auth-verdicts stamp we
// attach to every Fauna-to-Fauna delivery. The bridge AEAD-unwrapped
// the sender's submission token (the SPF-equivalent: cryptographic
// proof of identity) AND seals the body it read off that authenticated
// session straight to the recipient, with no hop in between (the
// DKIM-equivalent: the bytes cannot have been altered in transit).
// Therefore SPF = Pass, DKIM = Pass, DMARC = Pass by construction.
// ARC = None: origin-side mail carries no upstream
// Authentication-Results chain to extend.
//
// Returned in the shared-Rust (UniFFI) shape so the ONE verdict → bus-row
// mapping and the wire converter both read the same value; the wire form is
// `mailfauna.AuthVerdictsToWire` of it.
func faunaSubmissionVerdicts() mailfauna.AuthVerdicts {
	return mailfauna.AuthVerdicts{
		Dkim:  faunaCore.DkimVerdictPass{},
		Spf:   faunaCore.SpfVerdictPass,
		Dmarc: faunaCore.DmarcVerdictPass{},
		Arc:   faunaCore.ArcVerdictNone,
	}
}

// partitionRecipientsByLocalDomains splits rcpts into Fauna-domain
// candidates (domain matches any active mail_domains entry per
// docs/goal/behavior/mail-multidomain.md) and external addresses.
// Malformed addresses route to external — they were accepted at
// RCPT TO so they have a wire shape; the outbound worker's
// MX-resolve path is the right surface to fail them.
func partitionRecipientsByLocalDomains(rcpts []string, localDomains []string) (faunaCandidates, external []string) {
	for _, rcpt := range rcpts {
		_, domain, err := splitRcptAddress(rcpt)
		if err != nil {
			external = append(external, rcpt)
			continue
		}
		if containsFoldDomain(localDomains, domain) {
			faunaCandidates = append(faunaCandidates, rcpt)
		} else {
			external = append(external, rcpt)
		}
	}
	return faunaCandidates, external
}

// dedupLocalRecipients reduces the RCPT-time-resolved local recipients
// to the set Data should deliver to: deduplicated by actor_id, with the
// sender's own actor filtered out (the always-emitted Sent copy covers
// it). Resolution itself already happened at RCPT TO
// (submission.go::resolveLocalRecipient) — this is pure in-memory
// shaping, no RPC.
func dedupLocalRecipients(resolved []resolvedLocalRcpt, senderActorID []byte) []resolvedLocalRcpt {
	if len(resolved) == 0 {
		return nil
	}
	out := make([]resolvedLocalRcpt, 0, len(resolved))
	seen := make(map[string]struct{}, len(resolved)+1)
	if len(senderActorID) > 0 {
		seen[string(senderActorID)] = struct{}{}
	}
	for _, r := range resolved {
		key := string(r.actorID)
		if _, dup := seen[key]; dup {
			continue
		}
		seen[key] = struct{}{}
		out = append(out, r)
	}
	return out
}

// recipientNoKeyError is the typed replacement for the old
// `strings.Contains(err.Error(), "has not provisioned an MLS pubkey")`
// classification (smtp-server.md § Error / tempfail strategy). Two
// no-pubkey situations exist and they map to different SMTP codes:
// SuccessionPending distinguishes "this actor is a succession's
// successor and will provision a key within one sign-in" (451 tempfail)
// from "never onboarded" (550 permanent reject) — see
// wsrpc.RecipientSealKeys.SuccessionPending.
type recipientNoKeyError struct {
	SuccessionPending bool
}

func (e *recipientNoKeyError) Error() string {
	if e.SuccessionPending {
		return "recipient succeeded another identity and has not yet re-provisioned an MLS pubkey"
	}
	return "recipient has not provisioned an MLS pubkey"
}

// checkRecipientMLSPubkeyAtRcpt confirms a resolved local recipient has an
// MLS pubkey on file — the shared RCPT-time validation both the submission
// arm (resolveLocalRecipient) and the inbound MX arm (Rcpt) answer a
// key-less recipient with per-RCPT, rather than deferring the failure to
// DATA where it would fail the whole transaction after earlier recipients
// of the same multi-RCPT message already committed (security-review turn
// 418 § 4; smtp-server.md § Recipient handling on submission / § Error /
// tempfail strategy rows :288-289). Returns a non-nil *gosmtp.SMTPError
// when this ONE recipient should be rejected (550, permanent) or
// tempfailed (451, succession pending or transport error); the caller
// drops just that RCPT, leaving the rest of the envelope unaffected.
//
// mailNewIngest is forwarded to fetch_recipient_mls_pubkey unchanged — see
// FetchRecipientMLSPubkeyHybrid's doc for which value a given call site
// wants (the submission arm's RCPT-time check keeps its existing `true`;
// the inbound MX arm's new check uses `false`, since the DATA-time
// `ResolveRecipientSealKeys` call remains the one genuine per-delivery
// resolution for that arm).
func checkRecipientMLSPubkeyAtRcpt(ctx context.Context, caller wsrpc.Caller, actorID []byte, addr string, mailNewIngest bool, logger *slog.Logger) (mlsPubkey, mlkemEk []byte, smtpErr *gosmtp.SMTPError) {
	mlsPubkey, mlkemEk, successionPending, err := wsrpc.FetchRecipientMLSPubkeyHybrid(ctx, caller, actorID, mailNewIngest)
	if err != nil {
		if logger != nil {
			logger.Warn("mta: fetch_recipient_mls_pubkey transport error", "err", err, "rcpt", addr)
		}
		return nil, nil, &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 7, 1},
			Message:      "Recipient validation temporarily unavailable; try again later",
		}
	}
	if mlsPubkey == nil {
		if successionPending {
			return nil, nil, &gosmtp.SMTPError{
				Code:         451,
				EnhancedCode: gosmtp.EnhancedCode{4, 7, 1},
				Message:      fmt.Sprintf("Recipient %q's mailbox is being restored; try again later", addr),
			}
		}
		return nil, nil, &gosmtp.SMTPError{
			Code:         550,
			EnhancedCode: gosmtp.EnhancedCode{5, 1, 1},
			Message:      fmt.Sprintf("Recipient %q has no encryption key on file", addr),
		}
	}
	return mlsPubkey, mlkemEk, nil
}

// deliverToFaunaActor encrypts the submitted body + tokenized index
// hint to a single Fauna actor's MLS + index pubkeys and submits via
// the right inbound-write RPC. The sender's own actor routes to
// `submit_inbound_mail` (server flips is_own_submission=true → lands
// in Sent); every other Fauna actor routes to `ingest_inbound_mail`
// (server keeps is_own_submission=false → lands in INBOX/Junk per
// disposition — own-submission always passes "accept", so it's
// INBOX).
//
// The recipient's seal keys resolve through the single Phase-3 D2
// resolver (`wsrpc.ResolveRecipientSealKeys`) at delivery time — the
// RCPT-time resolution (resolveLocalRecipient) remains the
// deliverability *validation*, but the keys sealed to always come from
// the one epoch-indexable seam. A recipient with no provisioned key
// surfaces as a *recipientNoKeyError (permanent 550, or 451 tempfail
// when keys.SuccessionPending — smtp-server.md § Error / tempfail
// strategy) at the caller; transport / encryption failures surface as
// transient (451 4.7.0). This RCPT-time key should already be resolved
// (resolveLocalRecipient) — reaching a no-key result here means it was
// revoked between RCPT and DATA.
func (s *submissionSession) deliverToFaunaActor(
	ctx context.Context,
	actorID, rawBody, indexHintBytes, reportHash []byte,
	dedupKeys mailfauna.MailDedupKeyPair,
	senderDomain string,
	timestamp int64,
	isRoleAddress bool,
	senderAddress string,
) error {
	keys, err := wsrpc.ResolveRecipientSealKeys(ctx, s.backend.client, actorID)
	if err != nil {
		return fmt.Errorf("resolve recipient seal keys: %w", err)
	}
	if keys.MLSPubkey == nil {
		return &recipientNoKeyError{SuccessionPending: keys.SuccessionPending}
	}
	// Phase-3 D1: sealed at rest in BOTH storage modes — the design-(b)
	// plaintext-mode no-seal branch is deleted (one at-rest byte shape; the
	// nest core never holds a content key). The body seals post-quantum
	// X-Wing to the recipient's two key halves (the resolver refuses a key
	// missing either). PQ-6 seals the hint hybrid too (no longer
	// leaking the plaintext body word-set under HNDL); IndexHintMlkemEk passes
	// the ek only while the index key is the MLS-pubkey fallback, so it pairs
	// with the key the hint is sealed to.
	encryptedBody, err := mailfauna.EncryptToRecipientHybrid(rawBody, keys.MLSPubkey, keys.MlkemEk)
	if err != nil {
		return fmt.Errorf("encrypt body: %w", err)
	}
	encryptedIndexHint, err := mailfauna.EncryptToRecipientHybrid(
		indexHintBytes, keys.IndexPubkey,
		mailfauna.IndexHintMlkemEk(keys.IndexPubkey, keys.MLSPubkey, keys.MlkemEk))
	if err != nil {
		return fmt.Errorf("encrypt index hint: %w", err)
	}
	// The sealed size is the RFC822.SIZE floor the recipient sees — capture it
	// before the body may be moved off the request onto the byte plane.
	sealedBodyLen := uint32(len(encryptedBody))
	// The submission leg needs the reference path exactly as the inbound one
	// does. The perimeter now admits messages far larger than the 2 MiB frame,
	// so without this a multi-megabyte *submitted* message would seal, overflow
	// the frame, and have the transport sever the connection under it — the
	// permanent failure masquerading as a transient one that this design exists
	// to kill (smtp-server.md § Message size limits).
	inlineBody, bodyRef, err := mailstage.StageSealedBody(
		ctx, s.backend.client, s.backend.bytePlane, actorID, encryptedBody, encryptedIndexHint)
	if err != nil {
		return err
	}
	verdicts := faunaSubmissionVerdicts()
	params := wsrpc.IngestInboundMailParams{
		ActorID:            actorID,
		EncryptedBody:      inlineBody,
		BodyRef:            bodyRef,
		EncryptedIndexHint: encryptedIndexHint,
		PublicMetadata: wsrpc.PublicMailMetadata{
			Timestamp:      timestamp,
			CiphertextSize: sealedBodyLen,
			SenderDomain:   senderDomain,
		},
		Verdicts: mailfauna.AuthVerdictsToWire(verdicts),
		// Submission never invokes the scan gate (mail-content-scanning.md:
		// outbound is not ClamAV-scanned), so neither copy carries a verdict.
		// Said explicitly, not left to a default: the nest records what this
		// field says, and `clean` would be a scan that never ran.
		ClamavVerdict: mailfauna.ClamavVerdictToWire(mailfauna.ClamavVerdictNotScanned{}),
		// The submitter's envelope address. `reject` was already refused at
		// RCPT TO; this lets the nest recompute a `hold` and place the message
		// in a ward's held mailbox. Empty for the sender's own Sent copy.
		SenderAddress:   senderAddress,
		SpamScore:       0,
		SpamDisposition: "accept",
		IsRoleAddress:   isRoleAddress,
		ReportHash:      reportHash,
		DedupKey:        dedupKeys.DedupKey,
		EnvelopeKey:     dedupKeys.EnvelopeKey,
	}
	if bytes.Equal(actorID, s.actorID) {
		// The sender's own Sent copy is never perimeter-scored: no bus rows
		// (the nest records none for an empty array).
		_, err = wsrpc.SubmitInboundMail(ctx, s.backend.client, params)
		return err
	}
	// A colleague's copy carries the perimeter's bus rows, minted by the ONE
	// shared-Rust mapping from exactly what its per-kind fields say
	// (content-scoring.md § The scoring-metadata bus): the synthetic pass
	// verdicts above, a zero spam score, no rspamd run, and no ClamAV run —
	// so no `clamav` row, and no `message_scan_results` row nest-side either.
	// Bus and detail record agree by construction: neither claims a scan.
	params.Scores = mailfauna.PerimeterMailScoreRows(0, mailfauna.ClamavVerdictNotScanned{}, nil, verdicts)
	if _, err = wsrpc.IngestInboundMail(ctx, s.backend.client, params); err != nil {
		return err
	}
	// A colleague's invitation, submitted from their mail app, lands on this
	// recipient's calendar too — after their copy is delivered, never failing it.
	placeInboundInvite(ctx, s.backend.client, s.backend.logger, actorID, rawBody, senderAddress, timestamp)
	return nil
}

// dispatchFaunaRecipients delivers the submitted body to every resolved
// Fauna actor in `recipients` plus the sender's own actor (Sent
// copy). Returns the first error encountered as a *gosmtp.SMTPError —
// callers (submissionSession.Data) propagate it to the wire.
//
// `parsed.Subject + " " + parsed.BodyText` is tokenized once and the
// canonical bytes are reused across recipients — the same plaintext
// is HPKE-sealed under each recipient's distinct index pubkey, so the
// ciphertext per recipient is distinct even though the underlying
// canonical-token-set is shared.
//
// `timestamp` is the message's parsed Date header in unix seconds;
// `senderDomain` is the authenticated user's domain (which always
// equals `s.backend.domain` by D.3's MAIL FROM gate).
func (s *submissionSession) dispatchFaunaRecipients(
	rawBody []byte,
	parsed *mailfauna.ParsedMessage,
	recipients []resolvedLocalRcpt,
	senderDomain string,
	senderAddress string,
) error {
	indexHint := mailfauna.Tokenize(parsed.Subject + " " + parsed.BodyText)
	// Canonical report-hash, once per message over the same parsed fields the
	// tokenizer consumes — identical for every recipient of the same message
	// on every nest (report-sharing.md § Content identity).
	reportHash := mailfauna.ReportHash(parsed.Subject, parsed.BodyText)
	// Canonical dedup keys over the raw RFC 5322 bytes (mailbox-migration.md
	// § Key format), once per message like the report-hash above. `rawBody`
	// is the full message; the MTA stamps a Message-ID at submission per RFC
	// 6409 §8.3, so the dedup key takes the primary form.
	dedupKeys := mailfauna.MailDedupKeys(rawBody)
	timestamp := parsed.DateUnixSeconds
	// The authenticated-sender stamp for the Fauna-recipient copies
	// (smtp-server.md § Architectural rules → The `X-Fauna-*` namespace): the
	// envelope sender MAIL FROM validated as OWNED by the authenticated actor —
	// not the `From:` header, which Data() holds to the same ownership
	// predicate (mail-multidomain.md § From: header ownership) but which this
	// door does not alignment-check against the envelope. It
	// rides only the recipients' sealed copies: the relayed external message is
	// enqueued elsewhere from the unstamped body, and the Sent copy below
	// passes no stamp lines (it names no sender but the user themselves — the
	// same shape as the nest's seal_and_store_sent_copy).
	authSenderStamp := mailfauna.BuildAuthenticatedSenderStamp(senderAddress)
	deliverOne := func(actorID []byte, isRoleAddress bool, stampLines []string) error {
		ctx, cancel := context.WithTimeout(context.Background(), auth.RPCTimeout)
		defer cancel()
		// stampLines: the authenticated-sender stamp (recipients only) then
		// this recipient's matched-alias X-Fauna-Address-* headers, onto
		// their sealed copy (subaddress / wildcard / disposable / catch-all —
		// mail-aliases.md § Resolution order), mirroring the inbound path so the
		// MDA / client sees which alias matched. prependHeaders returns rawBody
		// unchanged when there are none (exact / role-address / the Sent copy pay
		// nothing).
		body := prependHeaders(rawBody, stampLines)
		if err := s.deliverToFaunaActor(ctx, actorID, body, indexHint.CanonicalBytes, reportHash, dedupKeys, senderDomain, timestamp, isRoleAddress, senderAddress); err != nil {
			// Over-quota → the recipient's mailbox is full → 552 5.2.2
			// permanent (imap-server.md § Quota enforcement points → inbound
			// delivery; a local-to-local submission ingests via the same RPC).
			// Role-address recipients never reach here (nest skips the
			// pre-check); the Sent copy is own-submission (exempt).
			if code, ok := wsrpc.RpcErrorCode(err); ok && code == wsrpc.CodeOverQuota {
				return mailboxFullError()
			}
			var noKeyErr *recipientNoKeyError
			if errors.As(err, &noKeyErr) {
				if noKeyErr.SuccessionPending {
					// A succession's successor hasn't re-provisioned a key
					// yet (succession-aftermath.md § Re-key scope) — the
					// window is bounded (first sign-in), so the sender's
					// MTA should retry rather than bounce
					// (smtp-server.md § Error / tempfail strategy).
					return &gosmtp.SMTPError{
						Code:         451,
						EnhancedCode: gosmtp.EnhancedCode{4, 7, 1},
						Message:      "Recipient's mailbox is being restored; try again later",
					}
				}
				return &gosmtp.SMTPError{
					Code:         550,
					EnhancedCode: gosmtp.EnhancedCode{5, 1, 1},
					Message:      "Recipient has no encryption key on file",
				}
			}
			// A size failure is permanent — it can never succeed on retry — so it
			// must not fall through to the 451 below and have the submitter's MUA
			// retry a doomed message for days. Mirrors the inbound ladder
			// (server.go § Data) exactly: same class, same code
			// (smtp-server.md § Message size limits).
			if errors.Is(err, mailstage.ErrMessageTooLarge) {
				return &gosmtp.SMTPError{
					Code:         552,
					EnhancedCode: gosmtp.EnhancedCode{5, 3, 4},
					Message:      "Message exceeds fixed size limit",
				}
			}
			// Surface the swallowed cross-binary delivery error — without
			// this the wire only ever shows the opaque 451, leaving an
			// admin (and the e2e harness) no way to tell a transient
			// nest blip from a wire-shape mismatch in the Sent-copy /
			// fauna-recipient submit path.
			if s.backend != nil && s.backend.logger != nil {
				if code, detail, ok := wsrpc.RpcErrorDetail(err); ok {
					s.backend.logger.Warn("mta: fauna recipient delivery failed (→451)",
						"err", err, "rpc_code", code, "rpc_details", detail)
				} else {
					s.backend.logger.Warn("mta: fauna recipient delivery failed (→451)", "err", err)
				}
			}
			return &gosmtp.SMTPError{
				Code:         451,
				EnhancedCode: gosmtp.EnhancedCode{4, 7, 0},
				Message:      "Mail delivery temporarily unavailable; try again later",
			}
		}
		return nil
	}
	for _, r := range recipients {
		if err := deliverOne(r.actorID, r.isRoleAddress, sealedCopyStampLines(authSenderStamp, r.headersToStamp)); err != nil {
			return err
		}
	}
	// Sent copy for the sender's own actor. Always submitted via
	// submit_inbound_mail; the nest handler's is_own_submission=true
	// branch routes to Sent (\Seen). dedupLocalRecipients() already
	// excluded sess.actorID from `recipients`, so this fires exactly
	// once; deliverToFaunaActor resolves the sender's own seal keys.
	if len(s.actorID) == 0 {
		// Defensive: AUTH should have stashed actorID. Don't crash on a
		// fixture that bypasses AUTH.
		return nil
	}
	// The Sent copy is own-submission (server flips is_own_submission=true →
	// exempt from the per-mailbox quota); isRoleAddress is irrelevant → false and
	// it carries no matched-alias stamp (it's the sender's own copy).
	return deliverOne(s.actorID, false, nil)
}
