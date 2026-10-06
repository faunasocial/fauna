package caldav

import (
	"bytes"
	"context"
	"encoding/hex"
	"strings"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// maybeFanOutAutoSchedule implements the server-side `calendar-auto-schedule`
// organizer fan-out (caldav-server.md § Server-side auto-schedule) on a PUT.
// After a successful organizer PUT it:
//
//   - fans an iMIP REQUEST to the new body's attendees, and
//   - fans an iMIP CANCEL to attendees who were on the PRIOR roster but not the
//     new one (RFC 5546 — a removed attendee keeps a stale invite otherwise).
//
// Both are built by the single-sourced shared-Rust mailfauna.BuildEventImipFromICS
// (the same impl a Fauna app uses) and delivered per-recipient by
// fanOutScheduling, which classifies each attendee: an email-reachable one rides
// the caller-scoped enqueue_outbound_mail queue, a mailbox-less Fauna one (CalDAV
// on, email off) rides the WS-RPC sealed MLS scheduling rail. So a stock CalDAV
// client (e.g. Apple Calendar with no Fauna app present) can add/remove
// attendees — including mailbox-less Fauna users — with no Fauna app in the
// loop.
//
// `priorICS` is the prior stored body (captured read-before-write in put.go,
// before the PUT overwrote it) — nil for a create, or when the session could
// not unseal it (no MLS snapshot provisioned), in which case the REQUEST still
// fires but the CANCEL diff is skipped. It is non-nil only for an organizer
// PUT (put.go gates the prior read on the new body's ORGANIZER), which is what
// keeps the CANCEL path from ever firing on a non-organizer PUT.
//
// The decryption happens in-session (the MDA already holds the organizer's
// per-session capability), so there is no standing capability — the threat
// model holds (caldav-server.md § Threat model).
//
// Best-effort: a scheduling-send failure must NEVER fail the PUT, which has
// already committed to nest. Errors are logged and swallowed.
func (b *Backend) maybeFanOutAutoSchedule(ctx context.Context, sess *davauth.Session, newICS, priorICS, uidHash []byte) {
	// dtstamp stays a parameter so the shared writer is pure/deterministic; the
	// MDA supplies the PUT time.
	dtstamp := time.Now().UTC().Format(time.RFC3339)
	authedOrganizer := sess.AuthedLocalPart() + "@" + sess.AuthedDomain()

	// An ATTENDEE's PUT is the "Responding" half: a calendar app answers an
	// invitation by re-storing the event with its own PARTSTAT changed, so a
	// changed answer owes the organizer an iMIP REPLY. The shared builder
	// returns nil whenever nothing is owed — the authed user is the organizer,
	// is not rostered, has not answered, or re-stored an unchanged answer — so
	// an attendee's calendar app syncing its copy sends nothing.
	if reply := mailfauna.BuildAttendeeReplyFromICS(string(newICS), priorICS, authedOrganizer, dtstamp); reply != nil {
		b.fanOutScheduling(ctx, sess, uidHash, "REPLY", reply.From, reply.Recipients, reply.RawRfc5322)
		return
	}

	// REQUEST to the new roster. Only the ORGANIZER fans out — a non-organizer
	// PUT (e.g. an attendee syncing their own copy of a shared event) must NOT
	// re-invite the roster, so we compare the parsed ORGANIZER (msg.From) to the
	// AUTH'd actor. nil msg → no ORGANIZER or no email-reachable attendee in the
	// new body (an empty roster leaves newRecipients nil → CANCEL-all below).
	var newRecipients []string
	if req := mailfauna.BuildEventImipFromICS(mailfauna.ITipRequest, string(newICS), dtstamp); req != nil &&
		strings.EqualFold(strings.TrimSpace(req.From), authedOrganizer) {
		newRecipients = req.Recipients
		b.fanOutScheduling(ctx, sess, uidHash, "REQUEST", req.From, req.Recipients, req.RawRfc5322)
	}

	// CANCEL to attendees on the prior roster but absent from the new one. The
	// prior is read for an attendee's PUT too (the REPLY diff above); the From
	// check below keeps it from ever cancelling on their behalf.
	if len(priorICS) == 0 {
		return
	}
	cancel := mailfauna.BuildEventImipFromICS(mailfauna.ITipCancel, string(priorICS), dtstamp)
	if cancel == nil || !strings.EqualFold(strings.TrimSpace(cancel.From), authedOrganizer) {
		return
	}
	removed := subtractRecipients(cancel.Recipients, newRecipients)
	if len(removed) > 0 {
		b.fanOutScheduling(ctx, sess, uidHash, "CANCEL", cancel.From, removed, cancel.RawRfc5322)
	}
}

// maybeFanOutCancelOnDelete fans an iMIP CANCEL to every email-reachable
// attendee of an event an organizer DELETEs (caldav-server.md § Server-side
// auto-schedule — the organizer must withdraw the meeting). `priorICS` is the
// body being deleted, captured read-before-delete in delete.go. Only the
// ORGANIZER cancels — an attendee deleting their own copy of a shared event
// must not cancel it for everyone. Best-effort: never fails the committed
// DELETE.
func (b *Backend) maybeFanOutCancelOnDelete(ctx context.Context, sess *davauth.Session, priorICS, uidHash []byte) {
	if len(priorICS) == 0 {
		return
	}
	dtstamp := time.Now().UTC().Format(time.RFC3339)
	cancel := mailfauna.BuildEventImipFromICS(mailfauna.ITipCancel, string(priorICS), dtstamp)
	if cancel == nil {
		// No ORGANIZER, or no email-reachable attendee — nothing to withdraw.
		return
	}
	authedOrganizer := sess.AuthedLocalPart() + "@" + sess.AuthedDomain()
	if !strings.EqualFold(strings.TrimSpace(cancel.From), authedOrganizer) {
		return
	}
	b.fanOutScheduling(ctx, sess, uidHash, "CANCEL", cancel.From, cancel.Recipients, cancel.RawRfc5322)
}

// enqueueScheduling submits one iMIP scheduling message (REQUEST or CANCEL) to
// the caller-scoped outbound queue AS THE ORGANIZER (on_behalf_of_actor = the
// AUTH'd actor). Best-effort: a send failure is logged + swallowed (the
// PUT/DELETE has already committed). `method` is a log label only.
func (b *Backend) enqueueScheduling(
	ctx context.Context, sess *davauth.Session, uidHash []byte,
	method, from string, recipients []string, raw []byte,
) {
	if len(recipients) == 0 {
		return
	}
	actorID := sess.ActorID()
	// The outbound queue's (original_msgid, recipient) index is non-unique, and a
	// PUT's REQUEST + CANCEL recipient sets are disjoint by construction, so the
	// shared per-event msgid never collides.
	msgID := hex.EncodeToString(uidHash) + "@" + sess.AuthedDomain()
	// nil stagedBody: iMIP scheduling messages are small and always ride inline;
	// only the SMTP submission leg stages an over-inline-budget body.
	if _, err := wsrpc.EnqueueOutboundMail(
		ctx, sess.Client(), msgID, from, recipients, raw, &actorID, nil,
	); err != nil {
		b.logger.Warn(
			"caldav auto-schedule: enqueue iMIP failed",
			"method", method,
			"uid_hash", hex.EncodeToString(uidHash),
			"recipients", len(recipients),
			"err", err,
		)
	}
}

// mailboxLessRecipient is a classified attendee that rides the WS-RPC sealed
// scheduling rail instead of email: a Fauna user with CalDAV enabled but email
// disabled (no mailbox), so the iMIP is sealed to their key package and
// delivered as a one-off MLS scheduling welcome. `address` is for logging;
// `actorIDHex` + `keyPackage` drive the seal + delivery.
type mailboxLessRecipient struct {
	address    string
	actorIDHex string
	keyPackage []byte
	// peerDomain is the recipient's nest domain when they live on a FOREIGN nest
	// (a cross-nest mailbox-less attendee) — threaded into deliver_sealed_scheduling
	// as `peer_domain` so the organizer's nest relays the sealed welcome there. Empty
	// for a same-nest recipient (C4), in which case the delivery stays same-nest.
	peerDomain string
}

// fanOutScheduling delivers one scheduling message (`raw`, a REQUEST or CANCEL
// iMIP) to `recipients`, routing each over the rail its classification picks:
// email-reachable attendees ride the caller-scoped outbound queue
// (enqueueScheduling, exactly as before), mailbox-less Fauna attendees ride the
// WS-RPC sealed MLS rail (deliverSealedScheduling). The classification is the
// only new nest traffic, and it fires only for local-domain attendees (the
// LocalDomains pre-filter), so an all-external roster behaves byte-for-byte like
// the pre-mailbox-less gateway. Best-effort throughout — a send failure on
// either rail is logged + swallowed (the PUT/DELETE has already committed).
// `method` is a log label; `from` the organizer address; `uidHash` keys the
// outbound msgid.
func (b *Backend) fanOutScheduling(
	ctx context.Context, sess *davauth.Session, uidHash []byte,
	method, from string, recipients []string, raw []byte,
) {
	if len(recipients) == 0 {
		return
	}
	emailRail, mailboxLess := b.classifyAutoScheduleRecipients(ctx, sess, recipients)
	// Observability for a best-effort, security-sensitive path that was otherwise
	// silent on the happy path: record how the roster split across the two rails so
	// a production "the invite never arrived" can be diagnosed from the MDA log.
	b.logger.Info(
		"caldav auto-schedule: roster classified",
		"method", method,
		"uid_hash", hex.EncodeToString(uidHash),
		"recipients", len(recipients),
		"email_rail", len(emailRail),
		"sealed_rail", len(mailboxLess),
	)
	b.enqueueScheduling(ctx, sess, uidHash, method, from, emailRail, raw)
	b.deliverSealedScheduling(ctx, sess, method, sess.ActorID(), from, mailboxLess, raw)
}

// classifyAutoScheduleRecipients splits `recipients` into the email rail and the
// mailbox-less sealed rail (caldav-server.md § Server-side auto-schedule, C4
// same-nest + the cross-nest extension). For each attendee `<local>@<domain>`:
//
//   - OFF-box domain (not in LocalDomains) → the shared resolver decides
//     (classifyOffBox → mailfauna.ClassifyAttendeeTransport, anon cross-nest
//     discovery DIRECT to the peer nest): a mailbox-less Fauna attendee on a
//     DIFFERENT, email-disabled nest → the CROSS-NEST sealed rail (peerDomain set,
//     KP fetched via the organizer's own-nest federation relay); every other
//     outcome (an external address, a mail-enabled Fauna handle on the peer, or any
//     discovery/KP miss) → email rail. This is the same email-vs-sealed decision the
//     native app send path makes — one source of truth (priority #2/#4).
//   - local domain, EMAIL-ENABLED nest, resolve_recipient Resolved/Forward →
//     email rail (the attendee has a mailbox or an admin forwarder; the
//     in-domain seal / forward path delivers it, same as before).
//   - local domain, EMAIL-DISABLED nest (a CalDAV-only deployment) → skip
//     resolve_recipient entirely (the canonical `<handle>@<domain>` alias every
//     CalDAV-enabled actor holds for AUTH would Resolve, but no MTA/INBOX
//     delivers there) and classify by Fauna reachability below.
//   - local domain, no mail route (Reject/Discard) OR email-disabled nest, AND
//     actor.by_handle resolves the local part to a Fauna actor with a fetchable
//     key package → same-nest mailbox-less sealed rail (peerDomain empty).
//   - anything else (unknown local handle, no key package, or any RPC error)
//     → email rail, so a classification miss degrades to today's behavior
//     rather than dropping the attendee.
//
// Cost: an off-box attendee now triggers one discovery probe (an anon connect to
// `<domain>`), unlike the pre-cross-nest path that short-circuited every off-box
// domain to email with no network. An external (non-Fauna) domain fails the anon
// connect fast → email; the probe is best-effort and off the committed-PUT fan-out
// path — the same cost the native app send path already pays. A local
// EMAIL-ENABLED nest runs resolve_recipient first so the common mail-enabled local
// attendee costs one read; its two extra reads (by_handle + keypackage.fetch) fire
// only for a local address with no mail route. On an email-disabled nest
// resolve_recipient is skipped — the canonical alias is an AUTH identity there, not
// a deliverable mailbox.
func (b *Backend) classifyAutoScheduleRecipients(
	ctx context.Context, sess *davauth.Session, recipients []string,
) (emailRail []string, mailboxLess []mailboxLessRecipient) {
	client := sess.Client()
	for _, addr := range recipients {
		local, domain, ok := splitAddress(addr)
		if !ok {
			b.logger.Debug("caldav auto-schedule: → email rail (malformed)", "addr", addr)
			emailRail = append(emailRail, addr)
			continue
		}
		if !b.isLocalDomain(domain) {
			// Off-box domain: a CROSS-NEST mailbox-less Fauna attendee (a Fauna actor
			// on a DIFFERENT, email-disabled nest) rides the sealed rail; every other
			// off-box outcome (an external address, or a mail-enabled Fauna handle on
			// the peer) is genuinely email-reachable. The decision is the shared
			// resolver (resolve_attendee_transport) reached over UniFFI — anonymous
			// cross-nest discovery DIRECT to the peer nest, the faithful mirror of the
			// native app send path (discovery is anonymous/public, so it needs no
			// nest signature → direct; only the authenticated key-package fetch +
			// sealed delivery below relay through the organizer's OWN nest). Best-effort:
			// any error or an "email" verdict degrades to the email rail — the old
			// behavior here was unconditional email, so a classification miss is
			// byte-for-byte backward-compatible.
			t, err := b.classifyOffBox(addr)
			if err != nil || t.Rail != "sealed" {
				b.logger.Debug("caldav auto-schedule: → email rail (off-box classify)", "addr", addr, "rail", t.Rail, "err", err)
				emailRail = append(emailRail, addr)
				continue
			}
			// A mailbox-less Fauna attendee on a foreign nest. Fetch their key package
			// through the organizer's OWN nest's federation relay (KeypackageFetch
			// carries the peer nest_url) to seal the welcome to.
			kp, err := wsrpc.KeypackageFetch(ctx, client, t.ActorIDHex, t.NestURL)
			if err != nil || kp == nil {
				b.logger.Debug("caldav auto-schedule: → email rail (off-box no key package)", "addr", addr, "actor", t.ActorIDHex, "kp_nil", kp == nil, "err", err)
				emailRail = append(emailRail, addr)
				continue
			}
			b.logger.Debug("caldav auto-schedule: → sealed rail (cross-nest mailbox-less)", "addr", addr, "actor", t.ActorIDHex, "peer", domain)
			mailboxLess = append(mailboxLess, mailboxLessRecipient{
				address:    addr,
				actorIDHex: t.ActorIDHex,
				keyPackage: kp,
				peerDomain: domain,
			})
			continue
		}
		// Local-domain attendee. On an email-ENABLED nest a mailbox / forwarder
		// (resolve_recipient Resolved/Forward) is genuinely email-reachable → email
		// rail. On an email-DISABLED nest (a CalDAV-only deployment) we SKIP this
		// short-circuit: every CalDAV-enabled actor still holds the canonical
		// `<handle>@<domain>` alias for AUTH (`ensure_canonical_handle_alias` fires
		// on the recipient-pubkey provision regardless of email), so
		// resolve_recipient would Resolve and wrongly pick the email rail — but no
		// MTA/INBOX delivers there, so a local Fauna attendee must ride the sealed
		// scheduling rail instead (caldav-server.md § Server-side auto-schedule —
		// email-disabled nest → WS-RPC sealed delivery; the same-nest mirror of the
		// cross-nest resolve_attendee_transport keying on the peer's email_enabled).
		if b.nestMailEnabled() {
			// Empty sender_address: this is a *routing probe* ("is this attendee
			// reachable by email?"), not a delivery, so the guardian mail gate
			// must not answer Reject here and silently reroute the attendee onto
			// the sealed rail. The gate applies to the iMIP delivery that follows.
			resolved, err := wsrpc.ResolveRecipient(ctx, client, local, domain, sess.AuthedDomain(), "")
			if err != nil ||
				resolved.Outcome == wsrpc.ResolveResolved ||
				resolved.Outcome == wsrpc.ResolveForward {
				b.logger.Debug("caldav auto-schedule: → email rail (resolve_recipient)", "addr", addr, "outcome", resolved.Outcome, "err", err)
				emailRail = append(emailRail, addr)
				continue
			}
		}
		// No mail route (or an email-disabled nest where the canonical alias is
		// AUTH-only). Is it a mailbox-less Fauna actor with a key package?
		actor, err := wsrpc.ActorByHandle(ctx, client, local)
		if err != nil {
			// ErrActorNotFound (not a Fauna handle) or a transport blip — fall
			// back to email rather than drop.
			b.logger.Debug("caldav auto-schedule: → email rail (by_handle miss)", "addr", addr, "handle", local, "err", err)
			emailRail = append(emailRail, addr)
			continue
		}
		kp, err := wsrpc.KeypackageFetch(ctx, client, actor.ActorID, "")
		if err != nil || kp == nil {
			// No fetchable key package ⇒ not currently reachable over MLS.
			b.logger.Debug("caldav auto-schedule: → email rail (no key package)", "addr", addr, "actor", actor.ActorID, "kp_nil", kp == nil, "err", err)
			emailRail = append(emailRail, addr)
			continue
		}
		b.logger.Debug("caldav auto-schedule: → sealed rail (mailbox-less)", "addr", addr, "actor", actor.ActorID)
		mailboxLess = append(mailboxLess, mailboxLessRecipient{
			address:    addr,
			actorIDHex: actor.ActorID,
			keyPackage: kp,
		})
	}
	return emailRail, mailboxLess
}

// classifyOffBox resolves an OFF-box (cross-nest) attendee's transport via the
// shared resolver's UniFFI face (mailfauna.ClassifyAttendeeTransport →
// fauna_client_caldav::resolve_attendee_transport with AnonAttendeeDiscovery):
// anonymous discovery DIRECT to the peer nest, the same email-vs-sealed decision
// the native app send path makes. The injectable classifyTransport seam (set
// only in the Go unit twin) replaces the production cgo call, which does real
// network and so can't run in a unit test.
func (b *Backend) classifyOffBox(addr string) (mailfauna.AttendeeTransport, error) {
	if b.classifyTransport != nil {
		return b.classifyTransport(addr)
	}
	return mailfauna.ClassifyAttendeeTransport(addr)
}

// deliverSealedScheduling seals the iMIP `raw` to each mailbox-less recipient's
// key package (mailfauna.BuildSchedulingDelivery — an ephemeral MLS signer, so
// nest sees only ciphertext) and ships the opaque bytes over the caller-scoped
// fauna.bridges.deliver_sealed_scheduling RPC AS THE ORGANIZER
// (`organizerActorID` = sess.ActorID(); `originalSender` = the organizer address
// nest resolves it against). `r.peerDomain` carries the rail cross-nest — empty for
// a same-nest recipient (C4), or the recipient's foreign nest domain for a
// cross-nest mailbox-less attendee, in which case the organizer's nest relays the
// welcome there. Best-effort per recipient — a seal/deliver failure is logged +
// swallowed.
func (b *Backend) deliverSealedScheduling(
	ctx context.Context, sess *davauth.Session, method string,
	organizerActorID []byte, originalSender string,
	recipients []mailboxLessRecipient, raw []byte,
) {
	for _, r := range recipients {
		delivery, err := mailfauna.BuildSchedulingDelivery(r.keyPackage, organizerActorID, raw)
		if err != nil {
			b.logger.Warn(
				"caldav auto-schedule: seal mailbox-less scheduling delivery failed",
				"method", method,
				"recipient", r.address,
				"err", err,
			)
			continue
		}
		if _, _, err := wsrpc.DeliverSealedScheduling(
			ctx, sess.Client(),
			organizerActorID, originalSender, r.actorIDHex,
			r.peerDomain, // "" same-nest; the recipient's foreign nest domain cross-nest
			delivery.ChannelIdHex, delivery.WelcomeBytes, delivery.AppEnvelope,
		); err != nil {
			b.logger.Warn(
				"caldav auto-schedule: deliver mailbox-less scheduling failed",
				"method", method,
				"recipient", r.address,
				"err", err,
			)
			continue
		}
	}
}

// splitAddress splits a bare mail address `local@domain` (the form
// BuildEventImipFromICS yields — `mailto:` already stripped) on its last `@`.
// ok is false for a malformed address (no `@`, empty local part, or empty
// domain), which the classifier treats as email-rail.
func splitAddress(addr string) (local, domain string, ok bool) {
	addr = strings.TrimSpace(addr)
	at := strings.LastIndex(addr, "@")
	if at <= 0 || at == len(addr)-1 {
		return "", "", false
	}
	return addr[:at], addr[at+1:], true
}

// subtractRecipients returns the elements of `a` not present in `b`
// (case-insensitive, order-preserving) — the attendees removed from a roster.
func subtractRecipients(a, b []string) []string {
	if len(a) == 0 {
		return nil
	}
	inB := make(map[string]struct{}, len(b))
	for _, x := range b {
		inB[strings.ToLower(strings.TrimSpace(x))] = struct{}{}
	}
	var out []string
	for _, x := range a {
		if _, ok := inB[strings.ToLower(strings.TrimSpace(x))]; !ok {
			out = append(out, x)
		}
	}
	return out
}

// priorRosterForAutoSchedule reads the prior stored event body of a scheduled
// event (one with an ORGANIZER) for the two diffs a PUT owes: the organizer's
// CANCEL-on-removal roster diff, and an attendee's REPLY diff (an answer that
// did not change since the stored copy owes the organizer nothing). The cheap
// ORGANIZER gate (parsed from the new body — single-sourced shared Rust) skips
// the read entirely on the common personal-event PUT. Returns nil for a
// personal event or any read miss.
func (b *Backend) priorRosterForAutoSchedule(
	ctx context.Context, sess *davauth.Session, calID, newICS, uidHash []byte,
) []byte {
	if mailfauna.ParseIcalOrganizer(string(newICS)) == nil {
		return nil // personal event — no scheduling
	}
	return b.fetchPriorEventICS(ctx, sess, calID, uidHash)
}

// fetchPriorEventICS reads + unseals the prior stored body of the event with
// `uidHash` (read-before-write/delete for the CANCEL roster diff). ONE read
// path (sealed-both-modes design D1): the sealed prior opens via
// OpenStoredRecord and the per-session opener. Best-effort:
//
//   - a missing calendar/event (a create, not an update) → nil;
//   - an open failure — including a session with no MLS snapshot on file
//     (nil opener), or an unsealed prior → nil (logged).
//
// Never fails the committed PUT/DELETE — the caller treats nil as "no prior".
func (b *Backend) fetchPriorEventICS(
	ctx context.Context, sess *davauth.Session, calID, uidHash []byte,
) []byte {
	var opener mailfauna.RecordOpener
	if o := sess.RecordOpener(); o != nil {
		opener = o
	}
	events, err := b.fetchAllEvents(ctx, sess, calID)
	if err != nil {
		// CalendarNotFound (a brand-new calendar) is a normal "no prior".
		return nil
	}
	for i := range events {
		if !bytes.Equal(events[i].UIDHash, uidHash) {
			continue
		}
		plaintext, err := mailfauna.OpenStoredRecord(opener, events[i].EncryptedBody)
		if err != nil {
			b.logger.Warn(
				"caldav auto-schedule: prior-event open failed",
				"uid_hash", hex.EncodeToString(uidHash),
				"err", err,
			)
			return nil
		}
		return plaintext
	}
	return nil // no prior event (a create)
}
