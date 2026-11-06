package carddav

import (
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// fixtureCardPubkey is the card X25519 pubkey fixtureMSEK derives — the key
// every card record a test seals goes to, because the session opener's card
// ring is that MSEK's (`owner-key-material.md` § Path B-sibling-6).
func fixtureCardPubkey() []byte {
	m, err := faunaFfi.DeriveRecipientDavMaterial(fixtureMSEK)
	if err != nil {
		panic(err)
	}
	return m.CardPubkey
}

// sealCardRecord seals `body` classically as a `card-record` to `pubkey` —
// the envelope every address-book body at rest is.
func sealCardRecord(body, pubkey []byte) ([]byte, error) {
	return mailfauna.SealRecordHybrid(mailfauna.RecordKindCard, body, pubkey, nil)
}
