// Package-level type aliases + thin wrappers for the shared-Rust IMAP
// ENVELOPE derivation (libs/fauna-mail/src/envelope.rs).
//
// The bridge's IMAP MDA arm consumes these to emit ENVELOPE FETCH
// responses without re-parsing the RFC 5322 body Go-side.

package mailfauna

import (
	faunaMail "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_mail"
)

// Envelope mirrors libs/fauna-mail/src/envelope.rs::Envelope. `Sender`
// and `ReplyTo` carry the From-fallback the RFC mandates so the bridge
// doesn't repeat the rule. `Date` is ISO-8601 / RFC 3339; `MessageId`
// and `InReplyTo` preserve angle brackets.
type Envelope = faunaMail.Envelope

// EnvelopeAddress is the (personal, mailbox, host) triple from
// RFC 9051 §7.5.2. Empty `Host` means the source address had no `@`.
type EnvelopeAddress = faunaMail.EnvelopeAddress

// DeriveEnvelope parses raw RFC 5322 bytes and returns the IMAP
// ENVELOPE shape. Re-parses internally via mail-parser.
func DeriveEnvelope(raw []byte) (Envelope, error) {
	return faunaMail.DeriveEnvelope(raw)
}

// FromMailboxes returns every mailbox the message's From: field names that
// carries both a local part and a domain, in header order, headers only
// (`fauna_mail::envelope::from_mailboxes`). The filter is
// SenderDomainWithEnvelopeFallback's, so the first entry's Host (lowercased)
// is the DKIM d= anchor the signer keys on — the submission door refuses a
// count other than one and then checks the one address is owned by the
// authenticated actor (mail-multidomain.md § From: header ownership).
// Mailbox keeps the header's case.
func FromMailboxes(raw []byte) []EnvelopeAddress {
	return faunaMail.FromMailboxes(raw)
}
