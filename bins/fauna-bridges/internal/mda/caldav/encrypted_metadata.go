package caldav

import (
	"fmt"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/fxamacker/cbor/v2"
)

// EncryptedCollectionMetadata is the decrypted shape of the
// `encrypted_metadata` blob stored in
// `bridge_caldav_calendars.encrypted_metadata`. The shape itself is
// MDA-local — nest stores the sealed bytes verbatim and never
// inspects the plaintext.
//
// Per `caldav-server.md` § Standard properties — these are the
// MUA-visible DAV properties the backend renders on PROPFIND.
// PROPPATCH (Phase E.4) round-trips through this struct: decrypt the
// existing blob, mutate fields, re-seal via SealCollectionMetadata.
type EncryptedCollectionMetadata struct {
	// Displayname is the human-readable calendar name (DAV
	// `displayname`). Apple Calendar / Thunderbird / Evolution all
	// render this in the calendar list.
	Displayname string `cbor:"displayname"`
	// Color is a 7-character hex string ("#RRGGBB") rendered as the
	// CalDAV `calendar-color` property. Apple Calendar's ACE extension
	// adds RGB-A; v1 stays opaque hex per goal doc § Out of scope.
	Color string `cbor:"color"`
	// Description is the optional CalDAV `calendar-description`
	// property. Empty string when the user hasn't set one.
	Description string `cbor:"description,omitempty"`
}

// SealCollectionMetadata CBOR-encodes `m` and HPKE-seals the result
// to `recipientPubkey` (the AUTH'd actor's MLS pubkey). Result is
// the opaque byte-string nest stores in
// `bridge_caldav_calendars.encrypted_metadata`.
//
// `mlkemEk` is the ML-KEM half of the AUTH'd actor's seal key
// (`sess.MlkemEk()`, cached at sign-in with the pubkey — the fetch refuses a
// key without it): the metadata body seals with the X-Wing post-quantum suite
// (leg D2a), like every other body seal
// (`mailfauna.EncryptToRecipientHybrid`). The native Fauna app opens
// either suite (self-describing blob).
//
// HPKE freshness per call — the same plaintext seals to different
// ciphertext on every invocation, so PROPPATCH idempotency is
// derived from ciphertext-byte equality only when the caller
// re-uses the previous ciphertext byte-for-byte (the normal "no
// PROPPATCH happened" path). Genuine PROPPATCH always produces new
// ciphertext and lands as `Updated` (Phase E.4).
func SealCollectionMetadata(m EncryptedCollectionMetadata, recipientPubkey, mlkemEk []byte) ([]byte, error) {
	// MUST be canonical DAG-CBOR (length-first key sort), NOT bare
	// cbor.Marshal (struct-field order). Native Fauna apps read this
	// same blob and decode it with a STRICT canonical decoder
	// (fauna-client-caldav `decode_strict`), which rejects struct-order
	// keys — so a non-canonical seal renders as 0 calendars on every native
	// Events page even though CalDAV MUAs read it fine (the MDA's own Go
	// decoder is permissive, so a same-language round-trip never caught it).
	// Regression-pinned by TestSealCollectionMetadataIsCanonicalDagCbor.
	plaintext, err := dagcbor.Marshal(m)
	if err != nil {
		return nil, fmt.Errorf("caldav: marshal collection metadata: %w", err)
	}
	ct, err := mailfauna.EncryptToRecipientHybrid(plaintext, recipientPubkey, mlkemEk)
	if err != nil {
		return nil, fmt.Errorf("caldav: seal collection metadata: %w", err)
	}
	return ct, nil
}

// UnsealCollectionMetadata HPKE-opens `ciphertext` via the AUTH'd actor's
// per-session record opener (built once at AUTH from the MLS-snapshot
// plaintext — the F2 shape, so each open skips the per-call snapshot
// re-parse) and CBOR-decodes the plaintext into EncryptedCollectionMetadata.
//
// STRICT open — the same strict open every record gets (the event-body
// serve path's mailfauna.OpenStoredRecord included): collection metadata
// rests sealed, so a shape-corrupt blob must error — never pass through
// verbatim. A nil opener means the user's primary client
// hasn't provisioned an MlsSnapshotBlob yet — surface that case as an error
// so the backend can log + drop the calendar from the PROPFIND response
// (mirrors the IMAP body-axis FETCH "missing snapshot" surfacing).
//
// Empty / nil ciphertext yields an empty metadata struct (treated as
// "no metadata stored yet" — the lazy-Personal flow never sees this,
// but downstream callers that pre-fetch CalendarEntry should tolerate
// it for forward compatibility).
func UnsealCollectionMetadata(
	ciphertext []byte,
	opener *mailfauna.MailRecordOpener,
) (EncryptedCollectionMetadata, error) {
	var m EncryptedCollectionMetadata
	if len(ciphertext) == 0 {
		return m, nil
	}
	if opener == nil {
		return m, fmt.Errorf(
			"caldav: unseal collection metadata: no MLS snapshot " +
				"provisioned for this actor — user's primary client " +
				"must call provision_mls_snapshot_blob before the MDA " +
				"can HPKE-open calendar metadata",
		)
	}
	plaintext, err := opener.Open(ciphertext)
	if err != nil {
		return m, fmt.Errorf("caldav: open collection metadata: %w", err)
	}
	if err := cbor.Unmarshal(plaintext, &m); err != nil {
		return m, fmt.Errorf("caldav: decode collection metadata: %w", err)
	}
	return m, nil
}
