package imap

import (
	"strings"
	"time"

	"github.com/emersion/go-imap/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// convertBodyStructure translates a shared-Rust derived BodyStructure
// (libs/fauna-mail/src/bodystructure.rs) into emersion's IMAP
// BodyStructure tree. The shared-Rust shape carries:
//
//   - upper-cased Type / Subtype, flat Parameters / DispositionParameters,
//     optional Encoding / Lines / Disposition, recursive Parts.
//
// Emersion expects:
//
//   - either *BodyStructureSinglePart (with optional Text/MessageRFC822/Extended
//     metadata) or *BodyStructureMultiPart (with Children + Extended).
//
// We never set BodyStructureMessageRFC822 — message/rfc822 nesting is
// deferred to a follow-up; today, message/rfc822 parts render as a
// single-part leaf with Type="MESSAGE", Subtype="RFC822" (the
// extension-data slot for the inner envelope stays nil).
func convertBodyStructure(in mailfauna.BodyStructure) imap.BodyStructure {
	if in.Type == "MULTIPART" {
		children := make([]imap.BodyStructure, 0, len(in.Parts))
		for _, p := range in.Parts {
			children = append(children, convertBodyStructure(p))
		}
		return &imap.BodyStructureMultiPart{
			Children: children,
			Subtype:  strings.ToLower(in.Subtype),
			Extended: &imap.BodyStructureMultiPartExt{
				Params:      mimeParamsToMap(in.Parameters),
				Disposition: dispositionFrom(in.Disposition, in.DispositionParameters),
			},
		}
	}
	out := &imap.BodyStructureSinglePart{
		Type:     strings.ToLower(in.Type),
		Subtype:  strings.ToLower(in.Subtype),
		Params:   mimeParamsToMap(in.Parameters),
		ID:       optString(in.Id),
		Encoding: strings.ToLower(optString(in.Encoding)),
		Size:     in.SizeOctets,
		Extended: &imap.BodyStructureSinglePartExt{
			Disposition: dispositionFrom(in.Disposition, in.DispositionParameters),
		},
	}
	if in.Description != nil {
		out.Description = *in.Description
	}
	if in.Lines != nil && strings.EqualFold(in.Type, "TEXT") {
		out.Text = &imap.BodyStructureText{NumLines: int64(*in.Lines)}
	}
	return out
}

func mimeParamsToMap(params []mailfauna.MimeParam) map[string]string {
	if len(params) == 0 {
		return nil
	}
	out := make(map[string]string, len(params))
	for _, p := range params {
		out[strings.ToLower(p.Name)] = p.Value
	}
	return out
}

func dispositionFrom(value *string, params []mailfauna.MimeParam) *imap.BodyStructureDisposition {
	if value == nil && len(params) == 0 {
		return nil
	}
	d := &imap.BodyStructureDisposition{Params: mimeParamsToMap(params)}
	if value != nil {
		d.Value = strings.ToLower(*value)
	}
	return d
}

func optString(p *string) string {
	if p == nil {
		return ""
	}
	return *p
}

// convertEnvelope maps the shared-Rust Envelope (RFC 3339 Date, raw
// Message-Id with angle brackets) to emersion's *imap.Envelope (Date
// as time.Time, Message-Id with angle brackets stripped). Per emersion's
// doc comment on imap.Envelope: "The In-Reply-To and Message-ID values
// contain message identifiers without angle brackets."
func convertEnvelope(in mailfauna.Envelope) *imap.Envelope {
	out := &imap.Envelope{
		From:    convertAddresses(in.From),
		Sender:  convertAddresses(in.Sender),
		ReplyTo: convertAddresses(in.ReplyTo),
		To:      convertAddresses(in.To),
		Cc:      convertAddresses(in.Cc),
		Bcc:     convertAddresses(in.Bcc),
	}
	if in.Subject != nil {
		out.Subject = *in.Subject
	}
	if in.Date != nil {
		if t, err := time.Parse(time.RFC3339, *in.Date); err == nil {
			out.Date = t
		}
	}
	if in.MessageId != nil {
		out.MessageID = stripAngleBrackets(*in.MessageId)
	}
	if in.InReplyTo != nil {
		raw := stripAngleBrackets(*in.InReplyTo)
		if raw != "" {
			// Multiple references are space-separated in the In-Reply-To
			// header per RFC 5322; split if present.
			out.InReplyTo = strings.Fields(raw)
		}
	}
	return out
}

func convertAddresses(in []mailfauna.EnvelopeAddress) []imap.Address {
	if len(in) == 0 {
		return nil
	}
	out := make([]imap.Address, 0, len(in))
	for _, a := range in {
		addr := imap.Address{Mailbox: a.Mailbox, Host: a.Host}
		if a.Personal != nil {
			addr.Name = *a.Personal
		}
		out = append(out, addr)
	}
	return out
}

// stripAngleBrackets is the inverse of mail-parser's
// "return raw header bytes" — the shared-Rust Envelope preserves the
// angle brackets so the bridge can choose its representation per
// downstream contract. IMAP ENVELOPE wants them stripped.
func stripAngleBrackets(s string) string {
	s = strings.TrimSpace(s)
	if strings.HasPrefix(s, "<") && strings.HasSuffix(s, ">") {
		return s[1 : len(s)-1]
	}
	return s
}
