package mailfauna

import (
	"bufio"
	"io"
	"mime"
	"mime/multipart"
	"net/mail"
	"net/textproto"
	"strings"
)

// DsnFacts is what a null-reverse-path (`MAIL FROM:<>`) delivery carries for
// the nest's guardian mail gate. Every field is empty when raw is not a genuine
// RFC 3464 delivery-status report, or names nothing extractable.
type DsnFacts struct {
	// OriginalMsgID is the Message-ID of the message being reported on, read
	// from the report's third part (`message/rfc822` carrying the returned
	// message, or `text/rfc822-headers` carrying just its headers — RFC 3464
	// § 2.1.2 requires one of them on a report that has the content).
	//
	// This is the fact the nest's guardian mail gate authorizes on: it delivers
	// a null-path message to a supervised recipient only when this id matches
	// one the ward's own outbound mail recorded — and only within that
	// record's consumption budget, because a Message-ID leaks beyond the
	// recipients: RFC 5322 threading carries it in References:/In-Reply-To: to
	// every later thread participant. The id (96 random bits + provenance tag
	// when a Fauna path minted it) bounds who can *name* it; the nest-side
	// budget bounds what naming it is *worth* (`family-safety.md` § The mail
	// gate).
	OriginalMsgID string
	// ReplyAddresses is every address appearing in the report's own
	// top-level reply-target headers (From, Sender, Reply-To, To, Cc, plus the
	// Mail-Reply-To/Mail-Followup-To that reply-oriented MUAs honor) —
	// ASCII-lowercased, deduplicated, header order. These are the only
	// addresses a one-click reply (or reply-all) to the report can be
	// addressed to, so the nest records them on a correlated delivery and
	// declines to auto-seed the ward's allowlist for them: a reply to a report
	// whose author chose the addresses must not convert one bounded delivery
	// into permanent allowlisted access (`family-safety.md` § The mail gate).
	//
	// Mail-Reply-To/Mail-Followup-To are included because a mutt-class MUA
	// silently addresses a one-click reply to them ahead of From/Reply-To — an
	// attacker who set one would otherwise route the ward's reply to an
	// un-recorded address and dodge the decline.
	//
	// Capped at one over the nest's plausibility bound: a genuine DSN
	// carries `From: MAILER-DAEMON@…` and rarely anything else, so an
	// over-stuffed list is truncated to a length the nest still reads as
	// over-stuffed — it holds the message rather than record a bloated set.
	// Empty when raw is not a genuine report (the nest then also holds).
	ReplyAddresses []string
}

// dsnReplyAddressCap is one over the nest's plausibility bound (16): a list
// truncated here still decodes nest-side as "more than 16" and is held —
// truncation must never disguise an over-stuffed report as a plausible one.
const dsnReplyAddressCap = 17

// DsnCorrelation reports what a genuine RFC 3464 delivery-status report says
// about the message it bounces. Genuine means the top-level Content-Type is
// `multipart/report` with `report-type=delivery-status` AND a
// `message/delivery-status` part is present.
//
// The MTA calls this only for a null-reverse-path delivery and forwards the
// result on `ingest_inbound_mail`. Returning a zero DsnFacts is therefore
// always safe: it can over-hold, never over-deliver.
func DsnCorrelation(raw []byte) DsnFacts {
	var facts DsnFacts
	msg, err := mail.ReadMessage(bufio.NewReader(strings.NewReader(string(raw))))
	if err != nil {
		return facts
	}
	mediaType, params, err := mime.ParseMediaType(msg.Header.Get("Content-Type"))
	if err != nil || !strings.EqualFold(mediaType, "multipart/report") {
		return facts
	}
	if !strings.EqualFold(params["report-type"], "delivery-status") {
		return facts
	}
	boundary := params["boundary"]
	if boundary == "" {
		return facts
	}
	// A report's parts are, in order: the human-readable notice, the
	// machine-readable delivery-status, and (when the reporting MTA kept it) the
	// returned message or its headers. Walk all of them — the delivery-status
	// part is what makes the message a report at all, so its absence voids both
	// facts even if an rfc822 part happened to parse.
	sawDeliveryStatus := false
	mr := multipart.NewReader(msg.Body, boundary)
	for {
		part, err := mr.NextPart()
		if err != nil {
			break
		}
		partType, _, err := mime.ParseMediaType(part.Header.Get("Content-Type"))
		if err != nil {
			continue
		}
		switch {
		case strings.EqualFold(partType, "message/delivery-status"):
			sawDeliveryStatus = true
		case strings.EqualFold(partType, "message/rfc822"),
			strings.EqualFold(partType, "text/rfc822-headers"):
			if facts.OriginalMsgID == "" {
				facts.OriginalMsgID = returnedMessageID(part)
			}
		}
	}
	if !sawDeliveryStatus {
		return DsnFacts{}
	}
	facts.ReplyAddresses = replyAddresses(msg.Header)
	return facts
}

// replyAddresses collects the ASCII-lowercased, deduplicated address set of
// the report's own top-level reply-target headers — what a reply/reply-all can
// be addressed to (see DsnFacts.ReplyAddresses). Unparseable headers are
// skipped, not fatal: a genuine DSN's `From: MAILER-DAEMON@…` parses, and a
// report that hides its addresses from the parser yields a smaller (or
// empty) set, which the nest treats as *less* deliverable, never more.
//
// The case-fold is ASCII-only on purpose: the nest re-normalizes both the
// recorded set and the outbound address it declines through
// `normalize_mail_address` (`to_ascii_lowercase`, db/family.rs). Folding here
// with Go's full-Unicode `strings.ToLower` would lower an uppercase non-ASCII
// local part the nest's ASCII fold leaves alone, so the recorded key and the
// decline lookup could disagree (SMTPUTF8/EAI) and the decline miss. Matching
// the nest's fold keeps the two sides provably equal.
func replyAddresses(hdr mail.Header) []string {
	var out []string
	seen := map[string]bool{}
	for _, name := range []string{
		"From", "Sender", "Reply-To", "To", "Cc",
		"Mail-Reply-To", "Mail-Followup-To",
	} {
		if hdr.Get(name) == "" {
			continue
		}
		list, err := hdr.AddressList(name)
		if err != nil {
			continue
		}
		for _, a := range list {
			addr := asciiLower(strings.TrimSpace(a.Address))
			if addr == "" || seen[addr] {
				continue
			}
			seen[addr] = true
			out = append(out, addr)
			if len(out) == dsnReplyAddressCap {
				return out
			}
		}
	}
	return out
}

// asciiLower lowercases only ASCII A–Z, leaving every other byte untouched —
// the exact fold Rust's `str::to_ascii_lowercase` applies, which the nest's
// `normalize_mail_address` uses on both the recorded correlated-origin set and
// the outbound address it declines. Using it here (rather than
// `strings.ToLower`, which also folds non-ASCII uppercase) keeps the address a
// reply-target header records byte-identical to what the nest will look up.
func asciiLower(s string) string {
	var b []byte
	for i := 0; i < len(s); i++ {
		c := s[i]
		if c >= 'A' && c <= 'Z' {
			if b == nil {
				b = []byte(s)
			}
			b[i] = c + ('a' - 'A')
		}
	}
	if b == nil {
		return s
	}
	return string(b)
}

// returnedMessageID reads the `Message-ID:` header of the message a report
// returns — the body of its `message/rfc822` part, or the bare header block of
// its `text/rfc822-headers` part. Both start with an RFC 5322 header block, so
// one header read serves both. "" when absent or malformed.
func returnedMessageID(r io.Reader) string {
	tp := textproto.NewReader(bufio.NewReader(r))
	hdr, err := tp.ReadMIMEHeader()
	// A truncated header block still yields the fields read so far, and a
	// Message-ID among them is as good as one from a complete block.
	if len(hdr) == 0 && err != nil {
		return ""
	}
	return strings.TrimSpace(hdr.Get("Message-Id"))
}
