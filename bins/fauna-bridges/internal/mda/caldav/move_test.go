package caldav

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"io"
	"net/http"
	"strings"
	"sync"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
	"github.com/fxamacker/cbor/v2"
)

// The second calendar a MOVE/COPY targets.
var moveDestCalendarID = func() []byte {
	sum := sha256.Sum256([]byte("move-test-destination"))
	return sum[:]
}()

// moveKey is the session key every MOVE/COPY test authenticates with, and the
// source event's body sealed to it (sealed once, so every moveSourceEntry()
// carries the same bytes): the session opens the body and re-seals it; the
// Fauna sidecar must travel byte for byte.
var moveKey = sync.OnceValues(func() (reportFixture, []byte) {
	leaf := faunaFfi.GenerateX25519Keypair()
	snapshot, err := faunaFfi.EncodeMlsSnapshotPlaintextV1([]faunaFfi.X25519Keypair{leaf})
	if err != nil {
		panic(err)
	}
	blob, err := faunaFfi.SealMlsSnapshotBlob(snapshot, fixtureActorID, fixtureMSEK)
	if err != nil {
		panic(err)
	}
	body, err := mailfauna.EncryptToRecipient([]byte("event-body"), leaf.Pubkey)
	if err != nil {
		panic(err)
	}
	return reportFixture{leaf: leaf, snapshotBlob: blob}, body
})

// The event resting in testCalendarID that every MOVE/COPY test moves.
var moveUIDHash = bytes.Repeat([]byte{0xab}, 32)

func moveSourceEntry() wsrpc.EventEntry {
	_, body := moveKey()
	ext := []byte("sealed-fauna-sidecar")
	return wsrpc.EventEntry{
		EventID:            bytes.Repeat([]byte{0x01}, 32),
		UIDHash:            moveUIDHash,
		EncryptedBody:      body,
		EncryptedIndexHint: []byte("sealed-index-hint"),
		ETag:               "0000000000000002",
		EncryptedFaunaExt:  &ext,
	}
}

// moveCaller wires AUTH plus per-calendar contents: the source calendar holds
// the event, the destination holds `destEvents` (nil ⇒ the destination
// calendar does not exist).
func moveCaller(t *testing.T, destEvents []wsrpc.EventEntry) *mockCaller {
	t.Helper()
	fx, _ := moveKey()
	caller := decryptCaller(t, fx)
	caller.queryEventsByCalendar = map[string][]wsrpc.EventEntry{
		hex.EncodeToString(testCalendarID): {moveSourceEntry()},
	}
	if destEvents != nil {
		caller.queryEventsByCalendar[hex.EncodeToString(moveDestCalendarID)] = destEvents
	}
	return caller
}

func sendMove(t *testing.T, baseURL, method, destination, overwrite string) (int, string) {
	t.Helper()
	req, err := http.NewRequest(method,
		eventURL(baseURL, testCalendarID, hex.EncodeToString(moveUIDHash)), nil)
	if err != nil {
		t.Fatal(err)
	}
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
	req.Header.Set("Destination", destination)
	if overwrite != "" {
		req.Header.Set("Overwrite", overwrite)
	}
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	return resp.StatusCode, string(body)
}

func destURL(baseURL string) string {
	return eventURL(baseURL, moveDestCalendarID, "anything")
}

// TestMoveReSealsTheEventThenRemovesTheSource pins caldav-server.md § Write
// surface / § Atomicity rule for a MOVE between the user's own calendars: the
// destination write carries a fresh seal of the event plus its unchanged
// sidecar, the source delete follows it, and the answer is 201.
func TestMoveReSealsTheEventThenRemovesTheSource(t *testing.T) {
	caller := moveCaller(t, []wsrpc.EventEntry{})
	baseURL, stop := startServer(t, caller)
	defer stop()

	status, body := sendMove(t, baseURL, "MOVE", destURL(baseURL), "F")
	if status != http.StatusCreated {
		t.Fatalf("MOVE status = %d (%s), want 201", status, body)
	}
	puts := caller.callsOf(wsrpc.MethodPutEventCiphertext)
	if len(puts) != 1 {
		t.Fatalf("put_event_ciphertext fired %d times, want 1", len(puts))
	}
	var put struct {
		CalendarID         []byte  `cbor:"calendar_id"`
		UIDHash            []byte  `cbor:"uid_hash"`
		EncryptedBody      []byte  `cbor:"encrypted_body"`
		EncryptedIndexHint []byte  `cbor:"encrypted_index_hint"`
		EncryptedFaunaExt  *[]byte `cbor:"encrypted_fauna_ext"`
	}
	if err := cbor.Unmarshal(puts[0].body, &put); err != nil {
		t.Fatal(err)
	}
	src := moveSourceEntry()
	if !bytes.Equal(put.CalendarID, moveDestCalendarID) || !bytes.Equal(put.UIDHash, moveUIDHash) {
		t.Errorf("the write went to calendar %x / uid %x, want the destination under the same uid", put.CalendarID, put.UIDHash)
	}
	// A fresh seal, never the source's bytes: identical ciphertext would share
	// one nest content record, and the source delete would take the moved
	// event's body with it.
	if len(put.EncryptedBody) == 0 || bytes.Equal(put.EncryptedBody, src.EncryptedBody) {
		t.Error("MOVE must re-seal the body, not copy the source's ciphertext")
	}
	if len(put.EncryptedIndexHint) == 0 || bytes.Equal(put.EncryptedIndexHint, src.EncryptedIndexHint) {
		t.Error("MOVE must re-seal the index hint, not copy the source's ciphertext")
	}
	if put.EncryptedFaunaExt == nil || !bytes.Equal(*put.EncryptedFaunaExt, *src.EncryptedFaunaExt) {
		t.Error("MOVE must carry the Fauna sidecar with the event")
	}
	dels := caller.callsOf(wsrpc.MethodDeleteEvent)
	if len(dels) != 1 {
		t.Fatalf("delete_event fired %d times, want 1 (the source)", len(dels))
	}
	var del struct {
		CalendarID []byte `cbor:"calendar_id"`
	}
	if err := cbor.Unmarshal(dels[0].body, &del); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(del.CalendarID, testCalendarID) {
		t.Errorf("MOVE deleted from calendar %x, want the source", del.CalendarID)
	}
}

// TestCopyLeavesTheSource: COPY is MOVE without the source delete.
func TestCopyLeavesTheSource(t *testing.T) {
	caller := moveCaller(t, []wsrpc.EventEntry{})
	baseURL, stop := startServer(t, caller)
	defer stop()

	if status, body := sendMove(t, baseURL, "COPY", destURL(baseURL), ""); status != http.StatusCreated {
		t.Fatalf("COPY status = %d (%s), want 201", status, body)
	}
	if n := len(caller.callsOf(wsrpc.MethodDeleteEvent)); n != 0 {
		t.Errorf("COPY deleted %d events, want 0", n)
	}
}

// TestFailedMoveLeavesTheSourceWhereItWas: a destination calendar that does
// not exist answers 409, and nothing is written or deleted.
func TestFailedMoveLeavesTheSourceWhereItWas(t *testing.T) {
	caller := moveCaller(t, nil)
	baseURL, stop := startServer(t, caller)
	defer stop()

	if status, body := sendMove(t, baseURL, "MOVE", destURL(baseURL), ""); status != http.StatusConflict {
		t.Fatalf("MOVE into a missing calendar = %d (%s), want 409", status, body)
	}
	if n := len(caller.callsOf(wsrpc.MethodPutEventCiphertext)) + len(caller.callsOf(wsrpc.MethodDeleteEvent)); n != 0 {
		t.Errorf("a failed MOVE made %d writes, want 0", n)
	}
}

// TestMoveRefusals: Overwrite F onto the same event is 412, and a
// destination outside the user's own home set is 403 — nothing written.
func TestMoveRefusals(t *testing.T) {
	caller := moveCaller(t, []wsrpc.EventEntry{moveSourceEntry()})
	baseURL, stop := startServer(t, caller)
	defer stop()

	if status, _ := sendMove(t, baseURL, "MOVE", destURL(baseURL), "F"); status != http.StatusPreconditionFailed {
		t.Errorf("Overwrite F onto an existing event = %d, want 412", status)
	}
	other := baseURL + "/caldav/someone-else@" + fixtureDomain + "/" + hex.EncodeToString(moveDestCalendarID) + "/x.ics"
	if status, _ := sendMove(t, baseURL, "MOVE", other, ""); status != http.StatusForbidden {
		t.Errorf("MOVE into another user's calendar = %d, want 403", status)
	}
	if n := len(caller.callsOf(wsrpc.MethodPutEventCiphertext)) + len(caller.callsOf(wsrpc.MethodDeleteEvent)); n != 0 {
		t.Errorf("refused MOVEs made %d writes, want 0", n)
	}
}

// TestMoveIntoAFullAccountIs507: the destination write refused over quota
// answers 507 with the DAV:quota-not-exceeded precondition, and the source is
// never deleted (caldav-server.md § Write surface's MOVE row, § Atomicity rule).
func TestMoveIntoAFullAccountIs507(t *testing.T) {
	caller := moveCaller(t, []wsrpc.EventEntry{})
	caller.putEventErrCode = wsrpc.CodeOverQuota
	baseURL, stop := startServer(t, caller)
	defer stop()

	status, body := sendMove(t, baseURL, "MOVE", destURL(baseURL), "")
	if status != http.StatusInsufficientStorage {
		t.Fatalf("MOVE into a full account = %d (%s), want 507", status, body)
	}
	if !strings.Contains(body, "<D:quota-not-exceeded/>") {
		t.Errorf("507 body = %q, want the DAV:quota-not-exceeded precondition", body)
	}
	if n := len(caller.callsOf(wsrpc.MethodDeleteEvent)); n != 0 {
		t.Errorf("an over-quota MOVE deleted %d events, want 0 (the source stays)", n)
	}
}
