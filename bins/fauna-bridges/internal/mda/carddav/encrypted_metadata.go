package carddav

import (
	"fmt"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/fxamacker/cbor/v2"
)

// EncryptedCollectionMetadata is the decrypted shape of the
// `encrypted_metadata` blob stored in
// `bridge_carddav_addressbooks.encrypted_metadata`. The shape itself is
// MDA-local — nest stores the sealed bytes verbatim and never inspects the
// plaintext. Twin of the CalDAV EncryptedCollectionMetadata (address books
// carry no color, so that field is absent).
//
// These are the MUA-visible DAV properties the backend renders on PROPFIND.
// The collection PROPPATCH pass (props.go) round-trips through this struct:
// decrypt the existing blob, mutate the recognized fields, re-seal via
// SealCollectionMetadata.
type EncryptedCollectionMetadata struct {
	// Displayname is the human-readable address-book name (DAV
	// `displayname`). Apple Contacts / Thunderbird / DAVx5 all render this
	// in the address-book list.
	Displayname string `cbor:"displayname"`
	// Description is the optional CardDAV `addressbook-description` property.
	// Empty string when the user hasn't set one.
	Description string `cbor:"description,omitempty"`
}

// SealCollectionMetadata CBOR-encodes `m` and HPKE-seals the result to
// `recipientPubkey` (the AUTH'd actor's MLS pubkey). Result is the opaque
// byte-string nest stores in `bridge_carddav_addressbooks.encrypted_metadata`.
//
// `mlkemEk` is the ML-KEM half of the AUTH'd actor's seal key
// (`sess.MlkemEk()`, cached at sign-in with the pubkey — the fetch refuses a
// key without it): the metadata body seals with the X-Wing post-quantum suite
// (leg D2a), like every other body seal
// (`mailfauna.EncryptToRecipientHybrid`). The native Fauna app opens either
// suite (self-describing blob).
//
// HPKE freshness per call — the same plaintext seals to different ciphertext on
// every invocation.
func SealCollectionMetadata(m EncryptedCollectionMetadata, recipientPubkey, mlkemEk []byte) ([]byte, error) {
	// MUST be canonical DAG-CBOR (length-first key sort), NOT bare
	// cbor.Marshal (struct-field order). Native Fauna apps read this
	// same blob and decode it with a STRICT canonical decoder, which rejects
	// struct-order keys — so a non-canonical seal renders as 0 address books
	// on every native Contacts page even though CardDAV MUAs read it fine
	// (the MDA's own Go decoder is permissive, so a same-language round-trip
	// never catches it). Regression-pinned by
	// TestSealCollectionMetadataIsCanonicalDagCbor.
	plaintext, err := dagcbor.Marshal(m)
	if err != nil {
		return nil, fmt.Errorf("carddav: marshal collection metadata: %w", err)
	}
	ct, err := mailfauna.EncryptToRecipientHybrid(plaintext, recipientPubkey, mlkemEk)
	if err != nil {
		return nil, fmt.Errorf("carddav: seal collection metadata: %w", err)
	}
	return ct, nil
}

// UnsealCollectionMetadata HPKE-opens `ciphertext` via the AUTH'd actor's
// per-session record opener (built once at AUTH from the MLS-snapshot
// plaintext — the F2 shape, so each open skips the per-call snapshot
// re-parse) and CBOR-decodes the plaintext into EncryptedCollectionMetadata.
//
// STRICT open — the same strict open every record gets: address-book metadata
// rests sealed, so a shape-corrupt blob must error — never pass through
// verbatim. A nil opener means the user's primary client hasn't provisioned an
// MlsSnapshotBlob yet — surface that case as an error so the backend can log +
// drop the address book from the PROPFIND response.
//
// Empty / nil ciphertext yields an empty metadata struct (treated as "no
// metadata stored yet").
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
			"carddav: unseal collection metadata: no MLS snapshot " +
				"provisioned for this actor — user's primary client " +
				"must call provision_mls_snapshot_blob before the MDA " +
				"can HPKE-open address-book metadata",
		)
	}
	plaintext, err := opener.Open(ciphertext)
	if err != nil {
		return m, fmt.Errorf("carddav: open collection metadata: %w", err)
	}
	if err := cbor.Unmarshal(plaintext, &m); err != nil {
		return m, fmt.Errorf("carddav: decode collection metadata: %w", err)
	}
	return m, nil
}
