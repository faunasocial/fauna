package mailfauna

// AutoReply (Sieve vacation) shared-Rust surface (`smtp-server.md` § Email
// filter rules). The loop-guard predicate and the RFC 5322 reply composer live
// in shared Rust (`fauna_mail::filter::auto_reply_decision` /
// `fauna_mail::outbound::autoreply::compose_auto_reply`, uniffi-exported) — the
// perimeter-scorer split, like `Evaluate`. This file is the single
// fauna_mail import surface for them.

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

// AutoReplyMessage is the input to the RFC 5322 auto-reply composer.
type AutoReplyMessage = faunaMail.AutoReplyMessage

// AutoReplyDecision applies the RFC 5230 §4.4/§4.6 + RFC 3834 loop guards. Pure:
// it reads only the envelope sender, the message headers, the delivery recipient
// address, and our hosted domains — all available at the plaintext floor.
func AutoReplyDecision(envelopeFrom string, headers []FilterHeader, recipientAddr string, localDomains []string) AutoReplyGate {
	return faunaMail.AutoReplyDecision(envelopeFrom, headers, recipientAddr, localDomains)
}

// ComposeAutoReply builds the RFC 5322 wire bytes for a vacation auto-reply
// (`Auto-Submitted: auto-replied`, null-sender-bound by the caller at enqueue).
func ComposeAutoReply(msg AutoReplyMessage) []byte {
	return faunaMail.ComposeAutoReply(msg)
}
