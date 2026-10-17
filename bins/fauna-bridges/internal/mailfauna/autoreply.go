package mailfauna

// AutoReply (Sieve vacation) shared-Rust surface (`email-filters.md`, the
// AutoReply action). The loop-guard predicate lives in shared Rust
// (`fauna_mail::filter::auto_reply_decision`, uniffi-exported) — the
// perimeter-scorer split, like `Evaluate`. The reply itself is composed by the
// nest, never here (`mail-multidomain.md` § From: header ownership → Per door,
// nest-side). This file is the single fauna_mail import surface for it.

import (
	faunaMail "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_mail"
)

// AutoReplyGate is the loop-guard decision for a matched AutoReply action.
type AutoReplyGate = faunaMail.AutoReplyGate

// AutoReplyGate values — server.go compares against Send and maps the suppress
// variants to a metric label.
const (
	AutoReplyGateSend                    = faunaMail.AutoReplyGateSend
	AutoReplyGateSuppressNullSender      = faunaMail.AutoReplyGateSuppressNullSender
	AutoReplyGateSuppressAutoSubmitted   = faunaMail.AutoReplyGateSuppressAutoSubmitted
	AutoReplyGateSuppressBulk            = faunaMail.AutoReplyGateSuppressBulk
	AutoReplyGateSuppressOwnDomain       = faunaMail.AutoReplyGateSuppressOwnDomain
	AutoReplyGateSuppressNotInRecipients = faunaMail.AutoReplyGateSuppressNotInRecipients
)

// AutoReplyDecision applies the RFC 5230 §4.4/§4.6 + RFC 3834 loop guards. Pure:
// it reads only the envelope sender, the message headers, the delivery recipient
// address, and our hosted domains — all available at the plaintext floor.
func AutoReplyDecision(envelopeFrom string, headers []FilterHeader, recipientAddr string, localDomains []string) AutoReplyGate {
	return faunaMail.AutoReplyDecision(envelopeFrom, headers, recipientAddr, localDomains)
}
