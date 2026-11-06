package caldav

import (
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// fixtureCalendarPubkey is the calendar X25519 pubkey fixtureMSEK derives —
// the key every calendar record a test seals goes to, because the session
// opener's calendar ring is that MSEK's (`owner-key-material.md` § Path
// B-sibling-5 → *Snapshot carriage*).
func fixtureCalendarPubkey() []byte {
	m, err := faunaFfi.DeriveRecipientDavMaterial(fixtureMSEK)
	if err != nil {
		panic(err)
	}
	return m.CalendarPubkey
}

// sealCalendarRecord seals `body` classically as a `calendar-record` to
// `pubkey` — the envelope every calendar body at rest is.
func sealCalendarRecord(body, pubkey []byte) ([]byte, error) {
	return mailfauna.SealRecordHybrid(mailfauna.RecordKindCalendar, body, pubkey, nil)
}
