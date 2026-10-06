// Package-level type aliases + thin wrappers for the shared-Rust IMAP
// BODYSTRUCTURE derivation (libs/fauna-mail/src/bodystructure.rs).
//
// The bridge's IMAP MDA arm consumes these to emit BODYSTRUCTURE / BODY
// FETCH responses without re-parsing the RFC 5322 body Go-side. The
// shared-Rust path is the priority-#2 default; this file is a one-call
// wrapper, not a re-implementation.

package mailfauna

import (
	faunaMail "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_mail"
)

// BodyStructure mirrors libs/fauna-mail/src/bodystructure.rs::BodyStructure.
// `Type` and `Subtype` are upper-cased per RFC 9051 §7.5.2 conventional
// form; multipart parts have a non-empty Parts slice and unused
// Encoding/SizeOctets/Lines fields.
type BodyStructure = faunaMail.BodyStructure

// MimeParam is one Content-Type / Content-Disposition parameter; names
// are upper-cased, values verbatim.
type MimeParam = faunaMail.MimeParam

// DeriveBodyStructure walks raw RFC 5322 / MIME bytes and returns the
// IMAP BODYSTRUCTURE tree. Re-parses internally via mail-parser.
func DeriveBodyStructure(raw []byte) (BodyStructure, error) {
	return faunaMail.DeriveBodyStructure(raw)
}
