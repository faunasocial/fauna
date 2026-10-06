package carddav

import (
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// TestSealCollectionMetadataIsCanonicalDagCbor pins that the address-book
// metadata the MDA seals (e.g. the lazy-Contacts collection created on first
// PROPFIND) is **canonical DAG-CBOR**, not Go-struct-field-order CBOR.
//
// Why this is load-bearing: native Fauna apps read the same
// `encrypted_metadata` blob the MDA writes and decode it with a *strict*
// canonical-dag-cbor decoder. If the MDA marshals the `{displayname,
// description}` map in struct-definition order rather than length-first key
// order, the Rust decoder rejects it and silently drops the address book — so an
// MDA-lazy-created Contacts book renders as **0 address books** on the native
// Contacts page even though CardDAV MUAs see it fine. SealCollectionMetadata
// must marshal via internal/dagcbor. Twin of the CalDAV terminator's
// TestSealCollectionMetadataIsCanonicalDagCbor.
func TestSealCollectionMetadataIsCanonicalDagCbor(t *testing.T) {
	fx := newReportFixture(t)
	// description left empty on purpose — the lazy-Contacts flow seals exactly
	// this shape, and an empty (omitempty) field must not perturb the canonical
	// key ordering of the keys that remain.
	sealed, err := SealCollectionMetadata(EncryptedCollectionMetadata{
		Displayname: "Contacts",
		Description: "",
	}, fx.leaf.Pubkey, nil) // nil ek = classical seal
	if err != nil {
		t.Fatalf("SealCollectionMetadata: %v", err)
	}

	plaintext, err := openSealedMetadata(t, sealed, fx)
	if err != nil {
		t.Fatalf("openSealedMetadata: %v", err)
	}

	if err := dagcbor.ValidateCanonical(plaintext); err != nil {
		t.Fatalf(
			"sealed address-book metadata is NOT canonical dag-cbor: %v\n"+
				"  → a native app's strict decoder rejects it and renders 0 "+
				"address books for an MDA-created collection. SealCollectionMetadata "+
				"must marshal via internal/dagcbor (length-first key sort).",
			err,
		)
	}
}

// fixtureRecordOpener builds the per-session F2 record opener the AUTH
// middleware caches server-side (davauth resolve): unwrap the fixture MSEK,
// decrypt the fixture snapshot blob, parse it ONCE via NewMailRecordOpener.
func fixtureRecordOpener(t *testing.T, fx reportFixture) *mailfauna.MailRecordOpener {
	t.Helper()
	blob := mustReadFixture(t, "wrapped_msek.bin")
	cap, err := mailfauna.UnwrapMLSBlob(
		blob, fixturePlainPassword,
		fixtureActorID, fixtureCredentialID, mailfauna.KdfKindArgon2id,
	)
	if err != nil {
		t.Fatalf("UnwrapMLSBlob: %v", err)
	}
	defer cap.Zeroize()
	snapshotPlaintext, err := cap.Decrypt(fx.snapshotBlob)
	if err != nil {
		t.Fatalf("cap.Decrypt: %v", err)
	}
	opener, err := mailfauna.NewMailRecordOpener(snapshotPlaintext)
	if err != nil {
		t.Fatalf("NewMailRecordOpener: %v", err)
	}
	t.Cleanup(opener.Zeroize)
	return opener
}

// TestSealUnsealCollectionMetadataRoundTrip exercises REAL MLS crypto over the
// seal→open path both fields survive: SealCollectionMetadata → the AUTH-shape
// per-session record opener → UnsealCollectionMetadata yields back the exact
// Displayname + Description.
func TestSealUnsealCollectionMetadataRoundTrip(t *testing.T) {
	fx := newReportFixture(t)
	want := EncryptedCollectionMetadata{
		Displayname: "Work",
		Description: "colleagues",
	}
	sealed, err := SealCollectionMetadata(want, fx.leaf.Pubkey, nil)
	if err != nil {
		t.Fatalf("SealCollectionMetadata: %v", err)
	}

	got, err := UnsealCollectionMetadata(sealed, fixtureRecordOpener(t, fx))
	if err != nil {
		t.Fatalf("UnsealCollectionMetadata: %v", err)
	}
	if got.Displayname != want.Displayname {
		t.Errorf("Displayname = %q, want %q", got.Displayname, want.Displayname)
	}
	if got.Description != want.Description {
		t.Errorf("Description = %q, want %q", got.Description, want.Description)
	}
}

// TestUnsealCollectionMetadataNilSnapshotErrors pins the drop-one-return-the-rest
// contract's foundation: a nil record opener (user's primary client hasn't
// provisioned an MLS snapshot yet) surfaces as an error rather than a panic or
// empty success, so ListAddressBooks can log + skip the book.
func TestUnsealCollectionMetadataNilSnapshotErrors(t *testing.T) {
	fx := newReportFixture(t)
	sealed, err := SealCollectionMetadata(EncryptedCollectionMetadata{Displayname: "X"}, fx.leaf.Pubkey, nil)
	if err != nil {
		t.Fatalf("SealCollectionMetadata: %v", err)
	}
	if _, err := UnsealCollectionMetadata(sealed, nil); err == nil {
		t.Fatal("UnsealCollectionMetadata with nil opener must error, got nil")
	}
}

// TestUnsealCollectionMetadataRejectsRawBytes pins the STRICT open:
// address-book metadata rests sealed, so a raw / shape-corrupt blob must
// ERROR — never pass through verbatim (the same strict open every record
// gets). Twin of the CalDAV terminator's test of the same name.
func TestUnsealCollectionMetadataRejectsRawBytes(t *testing.T) {
	fx := newReportFixture(t)
	raw := []byte("not-a-sealed-mail-record-envelope")
	if _, err := UnsealCollectionMetadata(raw, fixtureRecordOpener(t, fx)); err == nil {
		t.Fatal("UnsealCollectionMetadata on raw bytes must error (strict sealed shape), got nil")
	}
}
