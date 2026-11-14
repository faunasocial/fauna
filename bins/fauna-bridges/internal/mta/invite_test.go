package mta

import (
	"bytes"
	"context"
	"crypto/rand"
	"fmt"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// inviteKeyCaller answers the two key fetches placeInboundInvite makes from one
// MSEK's real key row and records the placement it is handed.
type inviteKeyCaller struct {
	mailPub, mailEk  []byte
	calPub, calEk    []byte
	withCalendarKey  bool
	placed           map[string]any
	placementsCalled int
}

func (c *inviteKeyCaller) Call(_ context.Context, method string, body any, reply any) error {
	var out any
	switch method {
	case wsrpc.MethodFetchRecipientMLSPubkey:
		r := map[string]any{"key": map[string][]byte{"mls_pubkey": c.mailPub, "mlkem_ek": c.mailEk}}
		if c.withCalendarKey {
			r["calendar_key"] = map[string][]byte{"mls_pubkey": c.calPub, "mlkem_ek": c.calEk}
		}
		out = r
	case wsrpc.MethodFetchRecipientIndexKey:
		out = map[string]any{"pubkey": nil}
	case wsrpc.MethodPlaceInboundInvite:
		c.placementsCalled++
		raw, err := cbor.Marshal(body)
		if err != nil {
			return err
		}
		if err := cbor.Unmarshal(raw, &c.placed); err != nil {
			return err
		}
		out = map[string]any{"outcome": "placed"}
	default:
		return fmt.Errorf("inviteKeyCaller: unexpected method %q", method)
	}
	raw, err := cbor.Marshal(out)
	if err != nil {
		return err
	}
	return cbor.Unmarshal(raw, reply)
}

// TestInviteSealsToTheCalendarKey pins the MTA's iMIP placement on the calendar
// key (owner-key-material.md § Path B-sibling-5 → *Protects*): the placed body
// is a `calendar-record` the calendar grant payload of the recipient's MSEK
// opens and the mail payload does not; and a recipient with no calendar key is
// not placed at all — never a calendar record under the mail key.
//
// Not parallel: it swaps the package-level reader.
func TestInviteSealsToTheCalendarKey(t *testing.T) {
	orig := inviteFromMail
	inviteFromMail = func([]byte, int64) *mailfauna.InboundInvite {
		return &mailfauna.InboundInvite{Uid: "uid-1", Ics: "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"}
	}
	t.Cleanup(func() { inviteFromMail = orig })

	msek := make([]byte, 32)
	if _, err := rand.Read(msek); err != nil {
		t.Fatalf("rand: %v", err)
	}
	dav, err := faunaFfi.DeriveRecipientDavMaterial(msek)
	if err != nil {
		t.Fatalf("DeriveRecipientDavMaterial: %v", err)
	}
	mail, err := faunaFfi.DeriveRecipientMailXwingMaterial(msek)
	if err != nil {
		t.Fatalf("DeriveRecipientMailXwingMaterial: %v", err)
	}
	mailKp, err := faunaFfi.DeriveRecipientHpkeKeypair(msek)
	if err != nil {
		t.Fatalf("DeriveRecipientHpkeKeypair: %v", err)
	}
	actor := bytes.Repeat([]byte{0x42}, 32)
	c := &inviteKeyCaller{
		mailPub: mailKp.Pubkey, mailEk: mail.MlkemEk,
		calPub: dav.CalendarPubkey, calEk: dav.CalendarMlkemEk,
		withCalendarKey: true,
	}
	placeInboundInvite(context.Background(), c, nil, actor, []byte("raw"), "organizer@example.org", 1_700_000_000)
	if c.placementsCalled != 1 {
		t.Fatalf("placements = %d, want 1", c.placementsCalled)
	}
	// Open through the recipient's own session opener (the MSEK's calendar ring).
	token := []byte("bearer-token-for-the-invite-test")
	wrapped, err := faunaFfi.SealWrappedMsekBlob(msek, actor, "default", "oauthbearer", token, nil)
	if err != nil {
		t.Fatalf("SealWrappedMsekBlob: %v", err)
	}
	capability, err := mailfauna.UnwrapMLSBlob(wrapped, token, actor, "default", mailfauna.KdfKindHkdf, "")
	if err != nil {
		t.Fatalf("UnwrapMLSBlob: %v", err)
	}
	defer capability.Zeroize()
	snapshot, err := faunaFfi.EncodeMlsSnapshotPlaintextV1([]faunaFfi.X25519Keypair{mailKp})
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextV1: %v", err)
	}
	opener, err := mailfauna.NewRecordOpener(capability, snapshot)
	if err != nil {
		t.Fatalf("NewRecordOpener: %v", err)
	}
	defer opener.Zeroize()

	body, _ := c.placed["encrypted_body"].([]byte)
	pt, err := mailfauna.OpenStoredRecord(opener, body)
	if err != nil {
		t.Fatalf("the recipient's calendar ring must open the placed invitation: %v", err)
	}
	if !bytes.Contains(pt, []byte("BEGIN:VCALENDAR")) {
		t.Fatalf("placed body = %q", pt)
	}
	if _, err := faunaFfi.OpenMailRecordWithKey(body, mail.CapabilitySecret, 0); err == nil {
		t.Fatal("the mail payload opened the placed invitation; it must be a calendar record")
	}
	hint, _ := c.placed["encrypted_index_hint"].([]byte)
	if _, err := mailfauna.OpenStoredRecord(opener, hint); err != nil {
		t.Fatalf("with no index key registered the hint seals to the calendar key: %v", err)
	}
	if _, err := faunaFfi.OpenMailRecordWithKey(hint, mail.CapabilitySecret, 0); err == nil {
		t.Fatal("the mail payload opened the placed hint; it must be a calendar record")
	}

	noCal := &inviteKeyCaller{mailPub: mailKp.Pubkey, mailEk: mail.MlkemEk}
	placeInboundInvite(context.Background(), noCal, nil, actor, []byte("raw"), "organizer@example.org", 1_700_000_000)
	if noCal.placementsCalled != 0 {
		t.Fatal("a recipient with no calendar key must not be placed")
	}
}

// TestInviteStagePanicKeepsDeliveredMail pins the MTA half of the non-ASCII datetime panic: the
// invitation stage runs AFTER the recipient's copy is delivered, so a panic
// crossing the UniFFI boundary from the shared-Rust reader must not unwind into
// go-smtp's connection-level recover — that would turn the delivered message
// into a 421 and every sender retry into a duplicate delivery. The transaction
// still succeeds (250) and the local copy is ingested exactly once.
//
// Not parallel: it swaps the package-level reader, and a sequential test
// finishes before any parallel test's body runs.
func TestInviteStagePanicKeepsDeliveredMail(t *testing.T) {
	calls := 0
	orig := inviteFromMail
	inviteFromMail = func([]byte, int64) *mailfauna.InboundInvite {
		calls++
		panic("hostile invitation")
	}
	t.Cleanup(func() { inviteFromMail = orig })

	mc := newMockCaller()
	s := forwardingSession(t, mc, bytes.Repeat([]byte{0x42}, 32), "alice@example.org")

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("a post-delivery invite panic must not fail the transaction: %v", err)
	}
	if calls != 1 {
		t.Fatalf("the invite stage ran %d times, want 1 (the seam was not reached)", calls)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("local copy ingested %d times, want exactly 1", got)
	}
}
