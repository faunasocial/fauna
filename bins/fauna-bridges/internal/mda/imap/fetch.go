package imap

import (
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"sort"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const fetchRPCTimeout = 30 * time.Second

// unopenableRecordPlaceholderSubject is the Subject of the placeholder
// message a body-axis FETCH serves for a record no key in the session's
// standing set opens (unopenableRecordPlaceholder).
const unopenableRecordPlaceholderSubject = "This message could not be opened"

// unopenableRecordPlaceholder is the fixed RFC 5322 message a body-axis FETCH
// serves in place of a record no key in the session's standing set opens —
// sealed to another recipient, or malformed or tampered at rest
// (`imap-server.md` § Body-section FETCH → *Unopenable records*, the IMAP twin
// of `mail-app-surface.md` § Inbound client receive → *Unopenable records*).
// BODYSTRUCTURE, ENVELOPE, BODY[…] and BINARY[…] are all derived from it by
// the same shared-Rust calls as a real message, so every section form answers
// consistently. Pure 7-bit ASCII with CRLF line ends, so BODY[] and BINARY[]
// agree byte-for-byte. The message deliberately carries no From, Date or
// Message-ID: the MUA then falls back to INTERNALDATE, which is the record's
// real one, and no identity is invented.
const unopenableRecordPlaceholder = "" +
	"Subject: " + unopenableRecordPlaceholderSubject + "\r\n" +
	"MIME-Version: 1.0\r\n" +
	"Content-Type: text/plain; charset=us-ascii\r\n" +
	"Content-Transfer-Encoding: 7bit\r\n" +
	"\r\n" +
	"This message is stored on your Fauna server, but none of your\r\n" +
	"account's keys can open it: it was sealed to a different recipient,\r\n" +
	"or its stored copy is damaged. The rest of your mailbox is not\r\n" +
	"affected.\r\n"

// fetchResponseWriter is the seam Session.fetch dispatches through
// per-message. Production wraps emersion's *imapserver.FetchResponseWriter;
// tests substitute a fake that records every Write* call.
//
// The body-axis methods (WriteBodyStructure, WriteEnvelope,
// WriteBodySection) cover the C.6 path; WriteBodySection returns an
// io.WriteCloser the caller fills with the raw section bytes (mirrors
// emersion's contract — the writer's Close MUST happen before any other
// data item is written for the same message).
type fetchResponseWriter interface {
	WriteUID(imap.UID)
	WriteFlags([]imap.Flag)
	WriteModSeq(uint64)
	WriteInternalDate(time.Time)
	WriteRFC822Size(int64)
	WriteBodyStructure(imap.BodyStructure)
	WriteEnvelope(*imap.Envelope)
	WriteBodySection(*imap.FetchItemBodySection, int64) io.WriteCloser
	// BINARY[…] (RFC 3516 / RFC 9051 §6.4.5): WriteBinarySection returns an
	// io.WriteCloser the caller fills with the CTE-decoded section bytes
	// (emitted as a `~{n}` literal8); WriteBinarySectionSize emits the
	// `BINARY.SIZE[…] <n>` decoded-octet count inline.
	WriteBinarySection(*imap.FetchItemBinarySection, int64) io.WriteCloser
	WriteBinarySectionSize(*imap.FetchItemBinarySectionSize, uint32)
	Close() error
}

// fetchWriter is the per-FETCH-command seam. Production wraps
// *imapserver.FetchWriter; tests substitute a fake. WriteVanishedEarlier
// is the FAUNA-FORK QRESYNC seam (third_party/go-imap/FORK.md): it emits
// `* VANISHED (EARLIER) <uid_set>` (RFC 7162 §3.2.5.1) before the
// changed-message FETCH responses for a `(CHANGEDSINCE n VANISHED)` UID
// FETCH.
type fetchWriter interface {
	CreateMessage(seqNum uint32) fetchResponseWriter
	WriteVanishedEarlier(uids imap.UIDSet) error
}

// The per-message open seam is `mailfauna.RecordOpener` — the single
// interface through which every per-message open (body-FETCH, body-axis
// SEARCH, and the \Junk spam-train hook) HPKE-opens a sealed
// `MailRecordEnvelope` (sealed by APPEND / MTA inbound via
// `EncryptToRecipient` to the AUTH'd actor's leaf init pubkey). In
// production, Session.recordOpener (the per-connection
// `*mailfauna.MailRecordOpener` constructed at AUTH from the actor's MLS
// snapshot) is the implementation; tests inject a stub. Stored-record
// opens route through `mailfauna.OpenStoredRecord(opener, stored)`, one
// uniform serve path (Phase-3 D1): every mail record rests sealed and
// HPKE-opens via the seam; an unsealed payload is refused (the envelope
// decode errors) and a nil opener refuses every record — no storage-mode
// branch anywhere on the serve path. Only the spam-MODEL open calls
// `opener.Open` directly.
//
// This is deliberately NOT `MLSCapability.Decrypt`. `Decrypt` AEAD-opens
// the *snapshot blob* under MSEK — used once at AUTH (auth.go) to recover
// the snapshot plaintext — a different cryptographic primitive. The
// record opener HPKE-opens each per-message envelope using the leaf
// X25519 secret parsed from that snapshot plaintext. Conflating the two
// (calling `Decrypt` on an `EncryptToRecipient` envelope) fails at
// runtime; every per-message open must route through this seam.

// Fetch implements imapserver.Session.Fetch. Phase C.5 landed the
// metadata-only path; C.6 lands the body-axis path: a body-axis FETCH
// (BODYSTRUCTURE / ENVELOPE / BODY[]) issues an additional
// `fauna.bridges.fetch_message_ciphertext` RPC per row, AEAD-decrypts
// the ciphertext under the session's MLS capability, derives the
// BodyStructure + Envelope via shared-Rust, and caches the derivation
// (LRU; keyed by actor_id + mailbox + uid_validity + uid). Plaintext
// is ephemeral and zeroized after each row's writer flushes; only the
// derived BodyStructure + Envelope cache.
func (s *Session) Fetch(w *imapserver.FetchWriter, numSet imap.NumSet, options *imap.FetchOptions) error {
	s.mu.Lock()
	// Explicit nil-interface dance: assigning a typed nil pointer to an
	// interface variable produces a non-nil interface holding a typed
	// nil, which would defeat the `opener == nil` guard in the seam.
	var opener mailfauna.RecordOpener
	if s.recordOpener != nil {
		opener = s.recordOpener
	}
	s.mu.Unlock()
	return s.fetchWithDecryptor(&fetchWriterAdapter{w: w}, numSet, options, opener)
}

// fetchWriterAdapter wraps emersion's *imapserver.FetchWriter to
// satisfy the fetchWriter seam.
type fetchWriterAdapter struct {
	w *imapserver.FetchWriter
}

func (a *fetchWriterAdapter) CreateMessage(seqNum uint32) fetchResponseWriter {
	return &fetchResponseAdapter{w: a.w.CreateMessage(seqNum)}
}

func (a *fetchWriterAdapter) WriteVanishedEarlier(uids imap.UIDSet) error {
	return a.w.WriteVanishedEarlier(uids)
}

type fetchResponseAdapter struct {
	w *imapserver.FetchResponseWriter
}

func (a *fetchResponseAdapter) WriteUID(u imap.UID)           { a.w.WriteUID(u) }
func (a *fetchResponseAdapter) WriteFlags(f []imap.Flag)      { a.w.WriteFlags(f) }
func (a *fetchResponseAdapter) WriteModSeq(m uint64)          { a.w.WriteModSeq(m) }
func (a *fetchResponseAdapter) WriteInternalDate(t time.Time) { a.w.WriteInternalDate(t) }
func (a *fetchResponseAdapter) WriteRFC822Size(n int64)       { a.w.WriteRFC822Size(n) }
func (a *fetchResponseAdapter) WriteBodyStructure(bs imap.BodyStructure) {
	a.w.WriteBodyStructure(bs)
}
func (a *fetchResponseAdapter) WriteEnvelope(env *imap.Envelope) { a.w.WriteEnvelope(env) }
func (a *fetchResponseAdapter) WriteBodySection(section *imap.FetchItemBodySection, size int64) io.WriteCloser {
	return a.w.WriteBodySection(section, size)
}
func (a *fetchResponseAdapter) WriteBinarySection(section *imap.FetchItemBinarySection, size int64) io.WriteCloser {
	return a.w.WriteBinarySection(section, size)
}
func (a *fetchResponseAdapter) WriteBinarySectionSize(section *imap.FetchItemBinarySectionSize, size uint32) {
	a.w.WriteBinarySectionSize(section, size)
}
func (a *fetchResponseAdapter) Close() error { return a.w.Close() }

// isMetadataOnly reports whether the FETCH options can be satisfied
// from `fauna.bridges.fetch_message_metadata` alone (no plaintext
// body required). RFC822.SIZE is satisfiable from `ciphertext_size`
// (a small approximation: encrypted-mode users see the wire size as
// their "message size", which Phase C accepts; per the encryption-
// at-rest target in `imap-server.md` § Read surface).
func isMetadataOnly(o *imap.FetchOptions) bool {
	if o == nil {
		return true
	}
	if o.BodyStructure != nil || o.Envelope ||
		len(o.BodySection) > 0 ||
		len(o.BinarySection) > 0 ||
		len(o.BinarySectionSize) > 0 {
		return false
	}
	return true
}

// fetch is the seam-friendly metadata-only FETCH implementation; the
// existing fetch_test.go suite drives it without standing up an
// opener. Body-axis options route through fetchWithDecryptor.
func (s *Session) fetch(w fetchWriter, numSet imap.NumSet, options *imap.FetchOptions) error {
	return s.fetchWithDecryptor(w, numSet, options, nil)
}

// fetchWithDecryptor is the unified FETCH seam: metadata-only paths
// pass opener=nil; body-axis paths pass the session's per-connection
// record opener (or a test stub). Stored records open through
// `mailfauna.OpenStoredRecord` — a nil opener errors only once a body
// FETCH actually reaches a record.
func (s *Session) fetchWithDecryptor(w fetchWriter, numSet imap.NumSet, options *imap.FetchOptions, opener mailfauna.RecordOpener) error {
	s.mu.Lock()
	actorID := s.actorID
	mailbox := s.selectedMailbox
	uidValidity := s.selectedUIDValidity
	client := s.client
	cache := s.cache
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: FETCH requires authenticated state")
	}
	if mailbox == "" {
		return errors.New("imap: FETCH requires a SELECTed mailbox")
	}
	if !isMetadataOnly(options) {
		// The shared-Rust extractor serves top-level sections (BODY[] / HEADER /
		// TEXT / HEADER.FIELDS[.NOT] / <partial>), numbered-part sections
		// (BODY[N], BODY[N.MIME], BODY[N.HEADER], BODY[N.TEXT], recursing into
		// multipart + message/rfc822), and BINARY[…] / BINARY.SIZE[…] (RFC 3516
		// CTE-decode). Only the top-level MIME specifier remains unimplemented —
		// reject it early so a confused client sees a clear error instead of a
		// wrong/empty body.
		if err := rejectSectionedBodyFetches(options); err != nil {
			return err
		}
		// No upfront opener/snapshot guard: each record opens in fetchOne
		// (`mailfauna.OpenStoredRecord`). A record with no opener (user's
		// primary client hasn't provisioned an MLS snapshot yet — AUTH
		// succeeds regardless) surfaces the missing-snapshot error at open
		// time. Phase-3 D1.
	}

	ctx, cancel := context.WithTimeout(context.Background(), fetchRPCTimeout)
	defer cancel()

	// QRESYNC VANISHED FETCH modifier (RFC 7162 §3.2.5.1): when the client
	// included `(CHANGEDSINCE n VANISHED)` and has QRESYNC enabled, emit
	// `* VANISHED (EARLIER) <uid_set>` for messages expunged since n —
	// before the changed-message FETCH responses. The expunged set comes
	// from list_messages(since_modseq=n) (bridge_imap_expunged tombstones).
	// VANISHED requires CHANGEDSINCE (the fork parser enforces this).
	if options != nil && options.Vanished && options.ChangedSince != 0 && s.qresyncActive() {
		since := int64(options.ChangedSince)
		lm, err := wsrpc.ListMessages(ctx, client, wsrpc.ListMessagesParams{
			ActorID:     actorID,
			Mailbox:     mailbox,
			SinceModseq: &since,
		})
		if err != nil {
			return err
		}
		if len(lm.ExpungedUIDs) > 0 {
			if err := w.WriteVanishedEarlier(uidSetFromUint32s(lm.ExpungedUIDs)); err != nil {
				return err
			}
		}
	}

	// F1 fast path: a `UID FETCH` of a bounded explicit set fetches just those
	// UIDs' metadata — each row carries its nest-computed seqNum — instead of
	// the whole mailbox (which cost 113ms p50 @1k and saturated the WS queue
	// before 10k). A SeqSet FETCH, or a dynamic/oversized UIDSet, still fetches
	// all (`subset == false`), where a row's position IS its sequence number.
	uidFilter, subset := explicitFetchUIDs(numSet)
	rows, err := wsrpc.FetchMessageMetadata(ctx, client, actorID, mailbox, uidFilter)
	if err != nil {
		if _, detail, ok := wsrpc.RpcErrorDetail(err); ok && detail != "" {
			return fmt.Errorf("fetch_message_metadata: %s", detail)
		}
		return err
	}
	sort.Slice(rows, func(i, j int) bool { return rows[i].UID < rows[j].UID })

	// CONDSTORE MODSEQ is emitted when the client named the MODSEQ data
	// item (or used CHANGEDSINCE — the parser sets both) or has enabled
	// CONDSTORE for the session (RFC 7162 §3.1.4.1).
	emitModSeq := (options != nil && options.ModSeq) || s.condStoreActive()

	// RFC 9051 §6.4.5 implicit \Seen: a non-PEEK BODY[…] / BINARY[…] FETCH
	// marks each fetched message \Seen (BODY.PEEK[…] / BINARY.PEEK[…] /
	// BINARY.SIZE[…] don't). The read path is otherwise non-mutating, so this
	// is the one place a FETCH writes. Collect the matched UIDs lacking \Seen
	// — mirroring the emit loop's numSet + CHANGEDSINCE filters against the
	// pre-set modseq — set \Seen on them in ONE batched store_flags(add), and
	// merge the returned flags + bumped modseq back into `rows` so the per-row
	// emit reflects post-set state (RFC 7162 §3.1.4 — set BEFORE emit, so the
	// client sees the new FLAGS/MODSEQ on the same response, not a follow-up
	// unsolicited FETCH). The write also fans out the BridgeMailboxState::Flags
	// IDLE push to sibling sessions (desired: a parallel MUA learns it was read).
	newlySeen := map[uint32]struct{}{}
	if wantsSeen(options) {
		var toSet []uint32
		for i, m := range rows {
			seqNum := uint32(i + 1)
			if subset {
				seqNum = m.SeqNum
			}
			if !numSetMatches(numSet, seqNum, imap.UID(m.UID)) {
				continue
			}
			if options.ChangedSince != 0 && uint64(m.Modseq) <= options.ChangedSince {
				continue
			}
			if !hasFlag(m.Flags, string(imap.FlagSeen)) {
				toSet = append(toSet, m.UID)
			}
		}
		if len(toSet) > 0 {
			reply, err := wsrpc.StoreFlags(ctx, client, wsrpc.StoreFlagsParams{
				ActorID: actorID,
				Mailbox: mailbox,
				UIDs:    toSet,
				Op:      wsrpc.StoreFlagsOpAdd,
				Flags:   []string{string(imap.FlagSeen)},
			})
			if err != nil {
				return err
			}
			updatedByUID := make(map[uint32]wsrpc.StoreFlagsResultEntry, len(reply.Updated))
			for _, e := range reply.Updated {
				updatedByUID[e.UID] = e
				newlySeen[e.UID] = struct{}{}
			}
			for i := range rows {
				if e, ok := updatedByUID[rows[i].UID]; ok {
					rows[i].Flags = e.Flags
					rows[i].Modseq = e.ModSeq
				}
			}
		}
	}

	for i, m := range rows {
		seqNum := uint32(i + 1)
		if subset {
			seqNum = m.SeqNum
		}
		if !numSetMatches(numSet, seqNum, imap.UID(m.UID)) {
			continue
		}
		// CONDSTORE CHANGEDSINCE (RFC 7162 §3.1.4): skip rows whose modseq
		// is at or below the supplied value — only changes since then ship.
		if options != nil && options.ChangedSince != 0 && uint64(m.Modseq) <= options.ChangedSince {
			continue
		}
		// Force-emit FLAGS for a row whose \Seen we just set implicitly, even
		// when the client didn't request FLAGS — Dovecot parity, so the MUA
		// learns the message is now read (imap-server.md § Bar: parity-or-better).
		_, forceFlags := newlySeen[m.UID]
		if err := s.fetchOne(ctx, w, seqNum, m, options, opener, actorID, mailbox, uidValidity, cache, client, emitModSeq, forceFlags); err != nil {
			return err
		}
	}
	return nil
}

// fetchOne handles one row: metadata fields always; body-axis fields
// when options request them. Splitting this out keeps the seam loop
// readable and gives the not-found / cache-miss / cache-hit branches
// distinct names.
func (s *Session) fetchOne(
	ctx context.Context,
	w fetchWriter,
	seqNum uint32,
	m wsrpc.MessageMeta,
	options *imap.FetchOptions,
	opener mailfauna.RecordOpener,
	actorID []byte,
	mailbox string,
	uidValidity uint32,
	cache *bodyStructureCache,
	client wsrpc.Caller,
	emitModSeq bool,
	forceFlags bool,
) error {
	wantsBody := !isMetadataOnly(options)

	// Body-axis path: try cache first; on miss, fetch + decrypt + derive.
	var (
		cachedBS  mailfauna.BodyStructure
		cachedEnv mailfauna.Envelope
		plaintext []byte
	)
	if wantsBody {
		key := bodyStructureCacheKey{
			actorID:     string(actorID),
			mailbox:     mailbox,
			uidValidity: uidValidity,
			uid:         m.UID,
		}
		bs, env, hit := cache.Get(key)
		if hit && !needsPlaintext(options) {
			cachedBS = bs
			cachedEnv = env
		} else {
			ct, err := wsrpc.FetchMessageCiphertext(ctx, client, actorID, m.MessageID)
			if err != nil {
				if errors.Is(err, wsrpc.ErrMessageNotFound) {
					// UID was expunged between the metadata RPC and
					// the ciphertext RPC; per RFC 9051 § 6.4.8 we
					// silently skip the row.
					return nil
				}
				return err
			}
			// The sealed bytes may not have come inline: an oversized body
			// rides the bulk-byte plane by reference (body_ref.go).
			sealed, err := SealedBodyOf(ctx, s.plane, ct)
			if err != nil {
				return err
			}
			// ONE uniform serve rule (Phase-3 D1): the sealed record
			// HPKE-opens via the per-connection opener; an unsealed
			// payload, or a record that fails to open (wrong key /
			// corruption), is never served verbatim — it is served as the
			// placeholder below. No opener at all is a SESSION condition
			// (no MLS snapshot on file) and still fails the FETCH: a
			// placeholder for every message would be cached by the MUA for
			// good. OpenStoredRecordAt (not
			// OpenStoredRecord) so an epoch-aware opener can try the
			// record's own candidate epoch keys (content-sealing-epochs
			// design § 4), classified off the record's seal instant
			// (stored_at, never InternalDate: for
			// imported mail InternalDate is historical while the seal keyed
			// off ingest-time now).
			if opener == nil {
				return errors.New("imap: FETCH: no MLS snapshot on file")
			}
			degraded := false
			pt, err := mailfauna.OpenStoredRecordAt(opener, sealed, ct.SealEpochBasisUnix())
			if err != nil {
				// A record no key in the session's standing set opens is a
				// CONTAINED condition (`imap-server.md` § Body-section FETCH
				// → *Unopenable records*): serve this one message degraded
				// and keep the FETCH going, never fail every message the
				// command spans.
				s.warnUnopenable(m, err)
				pt = []byte(unopenableRecordPlaceholder)
				degraded = true
			} else {
				defer zeroize(pt)
			}
			plaintext = pt

			derivedBS, err := mailfauna.DeriveBodyStructure(plaintext)
			if err != nil {
				return err
			}
			derivedEnv, err := mailfauna.DeriveEnvelope(plaintext)
			if err != nil {
				return err
			}
			cachedBS = derivedBS
			cachedEnv = derivedEnv
			// The placeholder's structure never enters the cache: a record
			// that opens later (a key the session gains at its next AUTH)
			// must be served for real, not shadowed by the placeholder.
			if !degraded {
				cache.Put(key, derivedBS, derivedEnv)
			}
		}
	}

	mw := w.CreateMessage(seqNum)

	if options == nil || options.UID {
		mw.WriteUID(imap.UID(m.UID))
	}
	// FLAGS: emitted when the client asked, OR force-emitted for a row whose
	// \Seen this FETCH just set implicitly (forceFlags) so the MUA learns the
	// message is now read even on a bare BODY[] (Dovecot parity).
	if (options != nil && options.Flags) || forceFlags {
		mw.WriteFlags(toIMAPFlags(m.Flags))
	}
	if options != nil && options.InternalDate {
		mw.WriteInternalDate(time.Unix(m.InternalDate, 0).UTC())
	}
	// RFC822.SIZE is the stored record's size for every message, a degraded
	// one included: a metadata-only FETCH never opens the record, so serving
	// the placeholder's length here would answer the same UID two ways
	// depending on what else the FETCH asked for.
	if options != nil && options.RFC822Size {
		mw.WriteRFC822Size(int64(m.CiphertextSize))
	}
	// CONDSTORE: emit `MODSEQ (<n>)` when the client requested it or has
	// CONDSTORE enabled — the FAUNA-FORK FetchResponseWriter.WriteModSeq
	// seam (vendored go-imap, third_party/go-imap/FORK.md).
	if emitModSeq {
		mw.WriteModSeq(uint64(m.Modseq))
	}
	if wantsBody {
		if options.BodyStructure != nil {
			mw.WriteBodyStructure(convertBodyStructure(cachedBS))
		}
		if options.Envelope {
			mw.WriteEnvelope(convertEnvelope(cachedEnv))
		}
		for _, section := range options.BodySection {
			// Extract the requested section from the decrypted plaintext via
			// shared-Rust (RFC 9051 §6.4.5; libs/fauna-mail/src/bodysection.rs).
			// BODY[] / HEADER / TEXT / HEADER.FIELDS[.NOT] / numbered parts /
			// <partial>. The non-PEEK \Seen side-effect (RFC 9051 §6.4.5) is
			// applied once in fetchWithDecryptor's pre-pass (it spans all body
			// fetches uniformly), not per-section here.
			sectionBytes, err := mailfauna.FetchBodySection(plaintext, bodySectionSpecFromIMAP(section))
			if err != nil {
				return err
			}
			ws := mw.WriteBodySection(section, int64(len(sectionBytes)))
			if _, err := ws.Write(sectionBytes); err != nil {
				_ = ws.Close()
				return err
			}
			if err := ws.Close(); err != nil {
				return err
			}
		}
		for _, section := range options.BinarySection {
			// BINARY[<part>] (RFC 3516 / RFC 9051 §6.4.5): the addressed part's
			// Content-Transfer-Encoding-decoded contents (no charset
			// conversion) via shared-Rust (libs/fauna-mail/src/bodysection.rs).
			// A CTE the server can't decode → tagged NO [UNKNOWN-CTE].
			sectionBytes, err := mailfauna.FetchBinarySection(plaintext, binarySectionSpecFromIMAP(section))
			if err != nil {
				return mapUnknownCTE(err)
			}
			ws := mw.WriteBinarySection(section, int64(len(sectionBytes)))
			if _, err := ws.Write(sectionBytes); err != nil {
				_ = ws.Close()
				return err
			}
			if err := ws.Close(); err != nil {
				return err
			}
		}
		for _, section := range options.BinarySectionSize {
			// BINARY.SIZE[<part>]: the decoded octet count of the same section.
			size, err := mailfauna.FetchBinarySize(plaintext, uint32Part(section.Part))
			if err != nil {
				return mapUnknownCTE(err)
			}
			mw.WriteBinarySectionSize(section, size)
		}
	}

	return mw.Close()
}

// warnUnopenable logs a record the session's standing key set cannot open —
// WARN the first time the Backend sees that record, DEBUG on every FETCH after
// (a MUA re-syncing the mailbox spans it again and again), keyed by the
// record's nest message id. The error text carries no plaintext: the open
// failed before any existed.
func (s *Session) warnUnopenable(m wsrpc.MessageMeta, err error) {
	if s.undecryptableWarn == nil {
		return
	}
	id := hex.EncodeToString(m.MessageID)
	s.undecryptableWarn.Log(s.logger, id,
		"imap: serving placeholder for a mail record no session key opens",
		"message_id", id, "uid", m.UID, "err", err)
}

// needsPlaintext reports whether the request requires the actual
// decrypted bytes (BODY[] / RFC822 / BINARY[…]) rather than just the
// derived BodyStructure + Envelope. A cache hit alone satisfies
// BODYSTRUCTURE + ENVELOPE but not section emission — those need
// plaintext. BINARY.SIZE[…] counts too: its decoded octet count is not
// derivable from the cached (encoded-size) BodyStructure.
func needsPlaintext(o *imap.FetchOptions) bool {
	return o != nil &&
		(len(o.BodySection) > 0 || len(o.BinarySection) > 0 || len(o.BinarySectionSize) > 0)
}

// wantsSeen reports whether a FETCH implicitly sets \Seen on the fetched
// messages (RFC 9051 §6.4.5): true iff at least one **non-PEEK** BODY[…] or
// BINARY[…] section is requested. BODY.PEEK[…] / BINARY.PEEK[…] suppress the
// side-effect; BINARY.SIZE[…] is a size query that never sets it; and
// metadata-only items (FLAGS / ENVELOPE / BODYSTRUCTURE / UID / RFC822.SIZE)
// are not "the body" and never set it. The predicate is the single gate for
// the implicit-\Seen pre-pass in fetchWithDecryptor.
func wantsSeen(o *imap.FetchOptions) bool {
	if o == nil {
		return false
	}
	for _, s := range o.BodySection {
		if s != nil && !s.Peek {
			return true
		}
	}
	for _, s := range o.BinarySection {
		if s != nil && !s.Peek {
			return true
		}
	}
	return false
}

// hasFlag reports whether the canonical IMAP flag `want` is present in the
// row's flag set. nest stores flags in canonical RFC 9051 token form (`\Seen`,
// …), so an exact match suffices (mirrors store.go's shouldFireJunkSignal).
func hasFlag(flags []string, want string) bool {
	for _, f := range flags {
		if f == want {
			return true
		}
	}
	return false
}

// rejectSectionedBodyFetches refuses the one BODY[section] form the shared-Rust
// extractor doesn't serve. Served (no reject):
//   - every top-level section — BODY[] (none), BODY[HEADER], BODY[TEXT],
//     BODY[HEADER.FIELDS (...)] / BODY[HEADER.FIELDS.NOT (...)] (both arrive as
//     Specifier=Header with the field lists populated) — plus a <partial>
//     substring on any of them;
//   - every numbered-part section — BODY[N], BODY[N.MIME], BODY[N.HEADER],
//     BODY[N.TEXT] (and HEADER.FIELDS on a part), recursing into multipart and
//     encapsulated message/rfc822 (RFC 9051 §6.4.5 part numbering); and
//   - every BINARY[...] / BINARY.SIZE[...] (RFC 3516 CTE-decode — handled by the
//     dedicated emit loops in fetchOne, which map an undecodable CTE to a
//     tagged NO [UNKNOWN-CTE]).
//
// Still rejected: the *top-level* MIME specifier (BODY[MIME] with no numbered
// part — MIME only has meaning for a part; the shared-Rust extractor returns
// Unsupported).
func rejectSectionedBodyFetches(o *imap.FetchOptions) error {
	if o == nil {
		return nil
	}
	for _, s := range o.BodySection {
		if s == nil {
			continue
		}
		if len(s.Part) == 0 && s.Specifier == imap.PartSpecifierMIME {
			return errors.New("imap: top-level BODY[MIME] FETCH requires a numbered part")
		}
	}
	return nil
}

// bodySectionSpecFromIMAP maps a parsed FETCH BODY[section] request onto the
// shared-Rust BodySectionSpec. PEEK isn't forwarded: it neither changes the
// extracted octets (the response token never carries `.PEEK`) nor is needed
// here for the \Seen side-effect — that is decided by wantsSeen(options),
// reading the Peek bit directly, in fetchWithDecryptor's pre-pass.
func bodySectionSpecFromIMAP(s *imap.FetchItemBodySection) mailfauna.BodySectionSpec {
	spec := mailfauna.BodySectionSpec{
		Specifier:       string(s.Specifier),
		HeaderFields:    s.HeaderFields,
		HeaderFieldsNot: s.HeaderFieldsNot,
	}
	if len(s.Part) > 0 {
		spec.Part = make([]uint32, len(s.Part))
		for i, p := range s.Part {
			spec.Part[i] = uint32(p)
		}
	}
	if s.Partial != nil {
		spec.Partial = &mailfauna.BodySectionPartial{
			Offset: uint64(s.Partial.Offset),
			Size:   uint64(s.Partial.Size),
		}
	}
	return spec
}

// binarySectionSpecFromIMAP maps a parsed FETCH BINARY[<part>] request onto the
// shared-Rust BinarySectionSpec. PEEK isn't forwarded, for the same reason as
// bodySectionSpecFromIMAP: the \Seen side-effect it gates is decided by
// wantsSeen(options) in fetchWithDecryptor's pre-pass, not here.
func binarySectionSpecFromIMAP(s *imap.FetchItemBinarySection) mailfauna.BinarySectionSpec {
	spec := mailfauna.BinarySectionSpec{Part: uint32Part(s.Part)}
	if s.Partial != nil {
		spec.Partial = &mailfauna.BodySectionPartial{
			Offset: uint64(s.Partial.Offset),
			Size:   uint64(s.Partial.Size),
		}
	}
	return spec
}

// uint32Part converts a go-imap part path ([]int) to the shared-Rust
// representation ([]uint32). An empty path stays nil (the whole-message case).
func uint32Part(part []int) []uint32 {
	if len(part) == 0 {
		return nil
	}
	out := make([]uint32, len(part))
	for i, p := range part {
		out[i] = uint32(p)
	}
	return out
}

// mapUnknownCTE translates the shared-Rust ParseError::UnknownCte (a BINARY[…]
// fetch of a part whose Content-Transfer-Encoding the server can't decode) into
// an IMAP `NO [UNKNOWN-CTE]` status response (RFC 9051 §6.4.5 — the server MUST
// fail the request rather than return undecoded bytes). Any other error — and
// nil — passes through unchanged.
func mapUnknownCTE(err error) error {
	if errors.Is(err, mailfauna.ErrUnknownCTE) {
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Code: imap.ResponseCodeUnknownCTE,
			Text: "Cannot decode part content-transfer-encoding",
		}
	}
	return err
}

// zeroize writes 0x00 to every byte of b. Best-effort: Go's GC may
// have already copied b elsewhere (via slice grow / cgo / append). The
// in-place wipe still eliminates the most common diagnostic-dump path
// (a heap snapshot taken after FETCH would no longer expose the
// plaintext on the goroutine stack).
func zeroize(b []byte) {
	for i := range b {
		b[i] = 0
	}
}

// maxExplicitFetchUIDs bounds the F1 UID-subset fast path: a `UID FETCH` of a
// bounded explicit set this size or smaller fetches just those UIDs' metadata
// (each row carrying its nest-computed seqNum) rather than the whole mailbox.
// Beyond it, the full-mailbox fetch is used — its cost is comparable at that
// size and it avoids splicing a giant literal IN-list into the nest-side query.
const maxExplicitFetchUIDs = 4096

// explicitFetchUIDs returns the explicit UID list to pass to
// fetch_message_metadata when numSet is a bounded UIDSet of at most
// maxExplicitFetchUIDs entries (the F1 fast path), and true. It returns
// (nil, false) for a SeqSet, a dynamic UIDSet (one containing "*", which we
// can't enumerate and would want the whole mailbox for anyway), or a set whose
// cardinality exceeds the cap — the caller then fetches the whole mailbox and
// numbers rows by position. The cardinality is summed from the ranges WITHOUT
// materializing them: a bounded-but-huge range like `1:4000000000` would
// otherwise enumerate billions of UIDs inside UIDSet.Nums().
func explicitFetchUIDs(numSet imap.NumSet) ([]uint32, bool) {
	us, ok := numSet.(imap.UIDSet)
	if !ok || us.Dynamic() {
		return nil, false
	}
	var total uint64
	for _, r := range us {
		if r.Start == 0 || r.Stop == 0 {
			return nil, false // defensive; Dynamic() already excludes "*"
		}
		lo, hi := r.Start, r.Stop
		if lo > hi {
			lo, hi = hi, lo
		}
		total += uint64(hi-lo) + 1
		if total > maxExplicitFetchUIDs {
			return nil, false
		}
	}
	if total == 0 {
		return nil, false
	}
	nums, ok := us.Nums()
	if !ok {
		return nil, false
	}
	out := make([]uint32, len(nums))
	for i, u := range nums {
		out[i] = uint32(u)
	}
	return out, true
}

// numSetMatches checks whether seqNum (or uid) is in the set; mirrors
// the legacy bridge's helper.
func numSetMatches(ns imap.NumSet, seqNum uint32, uid imap.UID) bool {
	switch v := ns.(type) {
	case imap.SeqSet:
		return v.Contains(seqNum)
	case imap.UIDSet:
		return v.Contains(uid)
	default:
		return true
	}
}

// toIMAPFlags maps a wire-string flag list to typed imap.Flag values.
// nest stores flags in canonical RFC 9051 token form (`\Seen`, …,
// keywords); the bridge passes them through untranslated.
func toIMAPFlags(in []string) []imap.Flag {
	out := make([]imap.Flag, len(in))
	for i, s := range in {
		out[i] = imap.Flag(s)
	}
	return out
}
