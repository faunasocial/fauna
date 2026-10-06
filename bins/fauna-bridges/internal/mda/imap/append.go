package imap

import (
	"context"
	"errors"
	"fmt"
	"io"
	"time"

	"github.com/emersion/go-imap/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailstage"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// appendRPCTimeout caps the wsrpc round-trip for one APPEND.  Bumped
// vs. the metadata-fetch timeout because a large sealed body may be
// staged as several chunk uploads on the bulk-byte plane before the
// APPEND RPC itself (mailstage.StageSealedBody), and the encrypted
// payload doubles once for the body and once for the index hint.  Since
// ceiling retirement there is no size ceiling in StageSealedBody; the
// product ceiling `max_message_bytes` is enforced authoritatively by
// nest's append handler (IMAP APPEND has no SMTP perimeter clamp), and
// its `fauna.email.too_large` reply maps to an IMAP BAD below.
const appendRPCTimeout = 60 * time.Second

// sealToRecipient is the package-level seal function used by Append.
// Indirected through a var so tests can record which pubkey was used
// for each call (the FFI's `unseal_mail_record` counterpart is not
// exposed in the Go bindings, so unsealing-in-test is not available;
// the seam is the simplest way to verify Session.Append seals to the
// right key for body vs index hint).  The default delegates to the
// real FFI primitive; tests use `t.Cleanup` to restore.
//
// It is the suite-deciding hybrid seal (mailfauna.EncryptToRecipientHybrid):
// the body passes the actor's published ML-KEM ek so it seals X-Wing when
// one exists (leg D2b), and PQ-6 passes the index hint the same ek (via
// mailfauna.IndexHintMlkemEk) so the hint seals X-Wing too while the index
// key is the MLS-pubkey fallback (no longer leaking the plaintext word-set
// under HNDL).
var sealToRecipient = mailfauna.EncryptToRecipientHybrid

// Append implements emersion/go-imap's Session.Append: seal-to-self
// upload of a single RFC 5322 literal to one of the actor's mailboxes.
// Per `docs/goal/behavior/imap-server.md` § Write surface and the
// architectural rule "MDA encrypts user mail to the actor's own MLS
// pubkey on APPEND" (design tracked internally).
//
// Flow:
//
//  1. Read the literal in full.
//  2. Parse RFC 5322 — needed for the From-header sender domain (the
//     SEARCH FROM axis the encrypted-mode index can index) and for
//     Subject + BodyText tokenization that feeds the search-index hint.
//  3. Tokenize Subject + BodyText.  The CanonicalBytes serialization
//     is what we seal to the index pubkey.
//  4. HPKE-Seal the raw body bytes to the actor's MLS pubkey.
//  5. HPKE-Seal the CanonicalBytes to the actor's index pubkey — with
//     the Phase E gap fallback to the MLS pubkey when no index pubkey
//     is provisioned (mirrors the MTA inbound path's `ingestForRecipient`
//     fallback exactly so the recipient's MDA opens the same shape
//     regardless of who put the message there).
//  6. Round-trip `fauna.bridges.append`.
//  7. Return `*imap.AppendData{UID, UIDValidity}` so emersion emits
//     UIDPLUS `OK [APPENDUID <validity> <uid>] APPEND completed` per
//     RFC 4315.
//
// MULTIAPPEND (RFC 9051 §6.3.12): emersion's `imapserver/append.go`
// reads exactly one literal per `handleAppend` call, so the wire-level
// multi-literal-in-one-command form isn't supported by this upstream
// version.  Multi-message uploads come in as N separate APPEND
// commands; each is its own RPC, preserving per-RPC transaction
// discipline (partial success returns the landed UIDs plus a final
// BAD/NO).  No bridge-side handling needed.
func (s *Session) Append(mailbox string, r imap.LiteralReader, opts *imap.AppendOptions) (*imap.AppendData, error) {
	// Defensive: emersion gates Append behind ConnStateAuthenticated, so
	// in production this branch is unreachable. Belt-and-suspenders for
	// direct unit-test callers and any future surface that bypasses
	// emersion's state machine.
	s.mu.Lock()
	actorID := s.actorID
	mlsPubkey := s.actorMLSPubkey
	mlkemEk := s.actorMlkemEk
	indexPubkey := s.actorIndexKey
	s.mu.Unlock()
	if actorID == nil || mlsPubkey == nil {
		return nil, errors.New("imap: APPEND requires an authenticated session")
	}

	// Bound the literal before buffering it. The MDA does not hold the admin's
	// `max_message_bytes` knob (nest is APPEND's authoritative gate against it,
	// mail-message-size.md § Message size limits), but no knob can exceed
	// MaxMessageBytesCeiling, so a literal past that is refused here, unread,
	// whatever the IMAP library does with the announced size. The read itself
	// is limited too, so a literal that under-announces its size cannot be
	// buffered past the ceiling either.
	maxLiteral := int64(mailfauna.MaxMessageBytesCeiling())
	if r.Size() > maxLiteral {
		return nil, &imap.Error{
			Type: imap.StatusResponseTypeBad,
			Text: "APPEND message exceeds a fixed size limit",
		}
	}
	body, err := io.ReadAll(io.LimitReader(r, maxLiteral+1))
	if err != nil {
		return nil, fmt.Errorf("imap: APPEND read literal: %w", err)
	}
	if int64(len(body)) > maxLiteral {
		return nil, &imap.Error{
			Type: imap.StatusResponseTypeBad,
			Text: "APPEND message exceeds a fixed size limit",
		}
	}
	if len(body) == 0 {
		return nil, &imap.Error{
			Type: imap.StatusResponseTypeBad,
			Text: "APPEND literal is empty",
		}
	}

	// Every door that files bytes the nest did not compose removes the reserved
	// `X-Fauna-*` delivery stamps before parsing or sealing them (smtp-server.md
	// § Architectural rules → The X-Fauna-* namespace): the SELECT-time scorer
	// reads `X-Fauna-Spam-Threshold` off the sealed copy as this message's Junk
	// tier whichever door filed it, so a MUA must not be able to APPEND one.
	// `X-Fauna-Forwarded-By` survives (mail-forwarding.md § Loop detection).
	// The dedup key below is still taken over the literal as filed, so it
	// agrees with the key a migration client derives over the same source bytes.
	literal := body
	body = mailfauna.StripFaunaHeaders(literal)

	parsed, err := mailfauna.ParseRFC5322(body)
	if err != nil {
		return nil, &imap.Error{
			Type: imap.StatusResponseTypeBad,
			Text: "Message rejected by parser",
		}
	}

	// Subject + BodyText is the body-axis the encrypted-mode SEARCH /
	// BODY / TEXT predicates match against; participants ride the
	// header-axis from the From-header alone in encrypted mode.
	hint := mailfauna.Tokenize(parsed.Subject + " " + parsed.BodyText)

	ctx, cancel := context.WithTimeout(context.Background(), appendRPCTimeout)
	defer cancel()

	// Per-seal-site epoch-aware pubkey fetch: APPEND is a genuine new-mail
	// at-rest producer, so its seal key resolves per APPEND with
	// mail_new_ingest=true — the standing key while the write gate ships
	// false (byte-identical to today), the recipient's current epoch key
	// post-flip, matching MTA ingest. The AUTH-time session cache is
	// fetched with mail_new_ingest=false because it also feeds the
	// DAV/spam-reseal sites that must STAY standing,
	// so a single per-session value cannot serve both — hence this
	// per-seal-site fetch. On a transient fetch failure, degrade to the
	// cached standing pubkey rather than bouncing the APPEND (§ 3's
	// never-bounce posture; a standing-sealed record stays readable by
	// every epoch-aware opener's standing arm).
	if freshPubkey, freshEk, _, err := wsrpc.FetchRecipientMLSPubkeyHybrid(
		ctx, s.client, actorID, true); err == nil && freshPubkey != nil {
		mlsPubkey = freshPubkey
		mlkemEk = freshEk
	} else if err != nil && s.logger != nil {
		s.logger.Warn("imap: APPEND: per-seal pubkey fetch failed; sealing under the cached standing key",
			"err", err)
	}

	// Phase E gap: production index-pubkey provisioning is not yet
	// shipped, so for any actor today FetchRecipientIndexKey returns
	// None — and finishAuth leaves `actorIndexKey` nil.  Fall back to
	// the (per-seal-fetched) MLS pubkey so APPEND works end-to-end now
	// and the hint shares the body's key schedule, exactly like
	// `seal_and_persist_local` sealing both under one resolved key; the
	// wire format stays stable when Phase E adds real provisioning (the
	// recipient's MDA opens both envelopes with the same shape).  Mirrors
	// `mta/server.go::ingestForRecipient`'s fallback exactly.
	if indexPubkey == nil {
		indexPubkey = mlsPubkey
	}

	// Phase-3 D1 (`2026-07-07-phase-3-sealed-both-modes-design.md`): sealed at
	// rest in BOTH storage modes — the design-(b) plaintext-mode no-seal
	// branch is deleted; one at-rest byte shape. The body seals X-Wing to
	// the actor's two key halves; the index hint does too while its key is
	// the MLS-pubkey fallback, and classically to a dedicated index key.
	encryptedBody, err := sealToRecipient(body, mlsPubkey, mlkemEk)
	if err != nil {
		return nil, fmt.Errorf("imap: APPEND encrypt body: %w", err)
	}
	encryptedHint, err := sealToRecipient(
		hint.CanonicalBytes, indexPubkey, mailfauna.IndexHintMlkemEk(indexPubkey, mlsPubkey, mlkemEk))
	if err != nil {
		return nil, fmt.Errorf("imap: APPEND encrypt index hint: %w", err)
	}

	timestamp := s.now().Unix()
	if opts != nil && !opts.Time.IsZero() {
		timestamp = opts.Time.Unix()
	}

	var flags []string
	if opts != nil && len(opts.Flags) > 0 {
		flags = make([]string, len(opts.Flags))
		for i, f := range opts.Flags {
			flags[i] = string(f)
		}
	}

	// APPEND has no SMTP envelope, so the sender domain comes from the
	// parsed From header alone.  An empty result is allowed by the nest
	// handler (per `bridge_routing.rs:AppendMessageRequest.sender_domain`
	// comment — "Empty string is allowed").
	senderDomain := mailfauna.SenderDomainWithEnvelopeFallback(body, "")

	// The sealed size is the RFC822.SIZE-equivalent floor and must stay the true
	// value even when the body leaves the request by reference — capture it
	// before staging (mirrors the MTA's pre-staging CiphertextSize capture).
	ciphertextSize := uint32(len(encryptedBody))

	// Decide how the sealed body crosses to nest: inline if it fits the 2 MiB
	// WS-RPC frame, otherwise staged on the bulk-byte plane and named by a plain
	// MailBodyRef (APPEND is already ciphertext, so no staged-envelope). Shared
	// with the MTA-ingest leg via mailstage.StageSealedBody so the switchover
	// predicate and chunk boundaries are identical across every producer of
	// already-sealed bytes (smtp-server.md § Message size limits). Without this,
	// an over-frame APPEND would seal, overflow the frame, and sever the
	// connection instead of storing.
	inlineBody, bodyRef, err := mailstage.StageSealedBody(
		ctx, s.client, s.plane, actorID, encryptedBody, encryptedHint)
	if err != nil {
		// A permanent size failure is a client upload the MUA must not retry →
		// BAD (not a transport sever). Anything else (a transient staging / plane
		// failure) surfaces as an error the session maps to NO.
		if errors.Is(err, mailstage.ErrMessageTooLarge) {
			return nil, &imap.Error{
				Type: imap.StatusResponseTypeBad,
				Text: "APPEND message exceeds a fixed size limit",
			}
		}
		return nil, fmt.Errorf("imap: APPEND stage body: %w", err)
	}

	// Canonical dedup keys over the raw literal, computed here because the
	// nest receives only the sealed body (mailbox-migration.md § Key format).
	// The nest records the pair in actor_message_dedup so a later import
	// dedup-hits mail this MUA filed; it never suppresses the APPEND.
	dedupKeys := mailfauna.MailDedupKeys(literal)
	reply, err := wsrpc.Append(ctx, s.client, wsrpc.AppendParams{
		ActorID:            actorID,
		Mailbox:            mailbox,
		Flags:              flags,
		EncryptedBody:      inlineBody,
		BodyRef:            bodyRef,
		EncryptedIndexHint: encryptedHint,
		Timestamp:          timestamp,
		CiphertextSize:     ciphertextSize,
		SenderDomain:       senderDomain,
		DedupKey:           dedupKeys.DedupKey,
		EnvelopeKey:        dedupKeys.EnvelopeKey,
	})
	if err != nil {
		// A message over the product ceiling `max_message_bytes` comes back as
		// the typed `fauna.email.too_large` error (nest is IMAP APPEND's
		// authoritative size gate since ceiling retirement — it has no SMTP
		// perimeter clamp). Map it to an IMAP BAD: a permanent size failure the
		// MUA must not retry (smtp-server.md § Message size limits).
		if code, ok := wsrpc.RpcErrorCode(err); ok && code == wsrpc.CodeMessageTooLarge {
			return nil, &imap.Error{
				Type: imap.StatusResponseTypeBad,
				Text: "APPEND message exceeds a fixed size limit",
			}
		}
		// RFC 9208 enforcement: an over-quota APPEND comes back as the
		// typed `fauna.bridges.over_quota` error → surface it as a
		// `NO [OVERQUOTA]` status response (imap-server.md § Quota
		// enforcement points) rather than a bare NO.
		return nil, mapOverQuota(err)
	}

	return &imap.AppendData{
		UID:         imap.UID(reply.UID),
		UIDValidity: reply.UIDValidity,
	}, nil
}
