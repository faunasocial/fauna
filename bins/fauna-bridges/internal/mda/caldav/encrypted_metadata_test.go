package caldav

import (
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// TestSealCollectionMetadataIsCanonicalDagCbor pins that the calendar
// metadata the MDA seals (e.g. the lazy-Personal collection created on
// first PROPFIND) is **canonical DAG-CBOR**, not Go-struct-field-order
// CBOR.
//
// Why this is load-bearing: native Fauna apps read the same
// `encrypted_metadata` blob the MDA writes and decode it with a *strict*
// canonical-dag-cbor decoder (Rust `fauna_core` via
// `caldav_backend::calendar_row_from_entry`). If the MDA marshals the
// `{displayname, color, description}` map in struct-definition order
// rather than length-first key order (`color`, `description`,
// `displayname`), the Rust decoder rejects it ("map keys not in
// length-first bytewise ascending order") and silently drops the
// calendar — so an MDA-lazy-created Personal renders as **0 calendars**
// on the linux/macOS/etc. Events page even though the CalDAV MUAs see it
// fine. (Caught by the three-client live CalDAV round-trip; the MDA's
// own Go decoder is permissive, so a same-language round-trip never
// exposed it.) SealCollectionMetadata must marshal via internal/dagcbor.
func TestSealCollectionMetadataIsCanonicalDagCbor(t *testing.T) {
	fx := newReportFixture(t)
	// description left empty on purpose — the lazy-Personal flow seals
	// exactly this shape, and an empty (omitempty) field must not perturb
	// the canonical key ordering of the keys that remain.
	sealed, err := SealCollectionMetadata(EncryptedCollectionMetadata{
		Displayname: "Personal",
		Color:       "#3273dc",
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
			"sealed calendar metadata is NOT canonical dag-cbor: %v\n"+
				"  → a native app's strict decoder rejects it and renders 0 "+
				"calendars for an MDA-created collection. SealCollectionMetadata "+
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
// seal→open path all three fields survive: SealCollectionMetadata → the
// AUTH-shape per-session record opener → UnsealCollectionMetadata yields back
// the exact Displayname + Color + Description. Twin of the CardDAV
// terminator's TestSealUnsealCollectionMetadataRoundTrip.
func TestSealUnsealCollectionMetadataRoundTrip(t *testing.T) {
	fx := newReportFixture(t)
	want := EncryptedCollectionMetadata{
		Displayname: "Work",
		Color:       "#ff5733",
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
	if got.Color != want.Color {
		t.Errorf("Color = %q, want %q", got.Color, want.Color)
	}
	if got.Description != want.Description {
		t.Errorf("Description = %q, want %q", got.Description, want.Description)
	}
}

// TestUnsealCollectionMetadataNilSnapshotErrors pins the drop-one-return-the-rest
// contract's foundation: a nil record opener (user's primary client hasn't
// provisioned an MLS snapshot yet) surfaces as an error rather than a panic or
// empty success, so ListCalendars can log + skip the calendar. Twin of the
// CardDAV terminator's TestUnsealCollectionMetadataNilSnapshotErrors.
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
// collection metadata rests sealed, so a raw / shape-corrupt blob must
// ERROR — never pass through verbatim (the same strict open every record,
// event bodies included, gets). Twin of the CardDAV terminator's test of
// the same name.
func TestUnsealCollectionMetadataRejectsRawBytes(t *testing.T) {
	fx := newReportFixture(t)
	raw := []byte("not-a-sealed-mail-record-envelope")
	if _, err := UnsealCollectionMetadata(raw, fixtureRecordOpener(t, fx)); err == nil {
		t.Fatal("UnsealCollectionMetadata on raw bytes must error (strict sealed shape), got nil")
	}
}
