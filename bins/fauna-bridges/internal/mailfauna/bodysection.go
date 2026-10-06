// Package-level type aliases + a thin wrapper for the shared-Rust IMAP
// BODY[<section>] extraction (libs/fauna-mail/src/bodysection.rs).
//
// RFC 9051 §6.4.5 sectioned FETCH. The MDA arm consumes this to emit
// BODY[HEADER] / BODY[TEXT] / BODY[HEADER.FIELDS (…)] / <partial> FETCH
// responses from the decrypted plaintext without re-parsing the RFC 5322
// body Go-side — the shared-Rust path is the priority-#2 default, sharing the
// one mail-parser parse with DeriveBodyStructure / DeriveEnvelope.

package mailfauna

import (
	faunaMail "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_mail"
)

// BodySectionSpec mirrors libs/fauna-mail/src/bodysection.rs::BodySectionSpec
// (itself a mirror of the go-imap fork's imap.FetchItemBodySection). Build it
// from the parsed FETCH request and hand it to FetchBodySection.
type BodySectionSpec = faunaMail.BodySectionSpec

// BodySectionPartial is the RFC 9051 §6.4.5 <offset.size> partial substring.
type BodySectionPartial = faunaMail.BodySectionPartial

// BinarySectionSpec mirrors libs/fauna-mail/src/bodysection.rs::BinarySectionSpec
// (a mirror of the go-imap fork's imap.FetchItemBinarySection). Build it from
// the parsed FETCH BINARY[…] request and hand it to FetchBinarySection.
type BinarySectionSpec = faunaMail.BinarySectionSpec

// ErrUnknownCTE is the shared-Rust ParseError variant a BINARY[…] FETCH returns
// when the addressed part's Content-Transfer-Encoding is one the server cannot
// decode (anything other than 7bit / 8bit / binary / quoted-printable /
// base64). The MDA maps it to a tagged `NO [UNKNOWN-CTE]` (RFC 9051 §6.4.5).
// Match it with errors.Is.
var ErrUnknownCTE = faunaMail.ErrParseErrorUnknownCte

// FetchBodySection extracts the requested BODY[<section>] octets from raw
// decrypted RFC 5322 bytes. Handles the top-level specifiers (BODY[] / HEADER /
// TEXT / HEADER.FIELDS[.NOT]) plus <partial>, and numbered-part addressing
// (BODY[N], BODY[N.MIME], BODY[N.HEADER], BODY[N.TEXT], recursing into
// multipart + message/rfc822 per RFC 9051 §6.4.5). The top-level MIME specifier
// returns ParseError::Unsupported (MIME only has meaning for a numbered part).
func FetchBodySection(raw []byte, spec BodySectionSpec) ([]byte, error) {
	return faunaMail.FetchBodySection(raw, spec)
}

// FetchBinarySection extracts the requested BINARY[<section-binary>] octets
// from raw decrypted RFC 5322 bytes (RFC 3516 / RFC 9051 §6.4.5): the addressed
// numbered part's contents with its Content-Transfer-Encoding decoded (base64 /
// quoted-printable), with NO charset conversion, plus an optional <partial>. An
// empty spec.Part returns the whole message (header + decoded body). A part
// whose CTE can't be decoded returns ErrUnknownCTE.
func FetchBinarySection(raw []byte, spec BinarySectionSpec) ([]byte, error) {
	return faunaMail.FetchBinarySection(raw, spec)
}

// FetchBinarySize returns the octet count of the CTE-decoded section
// FetchBinarySection would produce for the same part (RFC 9051 §6.4.5
// BINARY.SIZE[…]; carries no <partial>). Errors as FetchBinarySection.
func FetchBinarySize(raw []byte, part []uint32) (uint32, error) {
	return faunaMail.FetchBinarySize(raw, part)
}
