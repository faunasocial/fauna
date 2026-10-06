package imap

import (
	"bytes"
	"context"
	"errors"
	"testing"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// quotaCaller is the per-file test caller — a future drift on a
// sibling fixture (`searchCaller`, `fetchCaller`) doesn't break the
// C.8 coverage. Only intercepts `fauna.bridges.get_quota`; any other
// method is an unexpected-method error.
type quotaCaller struct {
	calls        []string
	gotActorID   []byte
	storageUsed  uint64
	messageUsed  uint32
	storageLimit uint64
	messageLimit uint32
	returnErr    error
}

func (q *quotaCaller) Call(_ context.Context, method string, body, reply any) error {
	q.calls = append(q.calls, method)
	if method != "fauna.bridges.get_quota" {
		return errors.New("quotaCaller: unexpected method " + method)
	}
	if q.returnErr != nil {
		return q.returnErr
	}
	// Snapshot the body's actor_id field through a real CBOR round-trip
	// so the test catches wrapper-side encoding bugs (the wsrpc.GetQuota
	// wrapper is responsible for translating the typed actor_id slice
	// to a CBOR byte-string, not a generic list).
	bb, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	var decoded struct {
		ActorID []byte `cbor:"actor_id"`
	}
	if err := cbor.Unmarshal(bb, &decoded); err != nil {
		return err
	}
	q.gotActorID = decoded.ActorID
	rep, err := dagcbor.Marshal(struct {
		StorageBytesUsed  uint64 `cbor:"storage_bytes_used"`
		MessageCountUsed  uint32 `cbor:"message_count_used"`
		StorageBytesLimit uint64 `cbor:"storage_bytes_limit"`
		MessageCountLimit uint32 `cbor:"message_count_limit"`
	}{
		StorageBytesUsed:  q.storageUsed,
		MessageCountUsed:  q.messageUsed,
		StorageBytesLimit: q.storageLimit,
		MessageCountLimit: q.messageLimit,
	})
	if err != nil {
		return err
	}
	return cbor.Unmarshal(rep, reply)
}

func TestSession_GetQuota_TranslatesNestReplyToQuotaData(t *testing.T) {
	c := &quotaCaller{
		storageUsed:  600,
		messageUsed:  3,
		storageLimit: 1 << 30, // 1 GiB
		messageLimit: 50_000,
	}
	actor := bytes32x(0xaa)
	s := &Session{
		client:          c,
		actorID:         actor,
		authedLocalPart: "alice",
	}

	data, err := s.GetQuota("user/alice")
	if err != nil {
		t.Fatalf("GetQuota: %v", err)
	}
	if len(c.calls) != 1 || c.calls[0] != "fauna.bridges.get_quota" {
		t.Fatalf("calls: %v", c.calls)
	}
	if !bytes.Equal(c.gotActorID, actor) {
		t.Errorf("actor_id wire: got %x, want %x", c.gotActorID, actor)
	}
	if data.Root != "user/alice" {
		t.Errorf("Root: %q, want user/alice", data.Root)
	}
	// RFC 9208 §3.2: STORAGE resource carries usage + limit in KiB
	// (1024 bytes per unit). The wrapper rounds bytes UP — a partial
	// KiB still counts as one toward usage, and a partial limit KiB
	// is impossible because the limit is a multiple of bytes too.
	stor, ok := data.Resources[imap.QuotaResourceStorage]
	if !ok {
		t.Fatalf("STORAGE resource missing; got %+v", data.Resources)
	}
	// 600 B rounds up → 1 KiB
	if stor.Usage != 1 {
		t.Errorf("STORAGE.Usage: %d KiB, want 1 (600 B rounded up)", stor.Usage)
	}
	// 1 GiB = 1<<20 KiB
	if stor.Limit != 1<<20 {
		t.Errorf("STORAGE.Limit: %d KiB, want %d (1 GiB)", stor.Limit, 1<<20)
	}
	// RFC 9208 §3.2: MESSAGE is a count.
	msg, ok := data.Resources[imap.QuotaResourceMessage]
	if !ok {
		t.Fatalf("MESSAGE resource missing; got %+v", data.Resources)
	}
	if msg.Usage != 3 || msg.Limit != 50_000 {
		t.Errorf("MESSAGE: usage=%d limit=%d, want 3/50000", msg.Usage, msg.Limit)
	}
}

func TestSession_GetQuota_ExactKiBBoundary(t *testing.T) {
	// 1024 B used → exactly 1 KiB on the wire (no double-rounding).
	c := &quotaCaller{
		storageUsed:  1024,
		messageUsed:  1,
		storageLimit: 2048, // 2 KiB
		messageLimit: 10,
	}
	s := &Session{
		client:          c,
		actorID:         bytes32x(0xbb),
		authedLocalPart: "bob",
	}
	data, err := s.GetQuota("user/bob")
	if err != nil {
		t.Fatalf("GetQuota: %v", err)
	}
	stor := data.Resources[imap.QuotaResourceStorage]
	if stor.Usage != 1 || stor.Limit != 2 {
		t.Errorf("STORAGE: usage=%d limit=%d KiB, want 1/2", stor.Usage, stor.Limit)
	}
}

func TestSession_GetQuota_ZeroUsedReportsZero(t *testing.T) {
	c := &quotaCaller{
		storageUsed:  0,
		messageUsed:  0,
		storageLimit: 1 << 30,
		messageLimit: 50_000,
	}
	s := &Session{
		client:          c,
		actorID:         bytes32x(0xcc),
		authedLocalPart: "carol",
	}
	data, err := s.GetQuota("user/carol")
	if err != nil {
		t.Fatalf("GetQuota: %v", err)
	}
	stor := data.Resources[imap.QuotaResourceStorage]
	if stor.Usage != 0 {
		t.Errorf("STORAGE.Usage: %d, want 0", stor.Usage)
	}
	msg := data.Resources[imap.QuotaResourceMessage]
	if msg.Usage != 0 {
		t.Errorf("MESSAGE.Usage: %d, want 0", msg.Usage)
	}
}

func TestSession_GetQuota_RequiresAuth(t *testing.T) {
	// Pre-AUTH session (actorID == nil): the method must refuse to
	// call nest. Hitting nest with a zero actor_id would surface the
	// fallback all-zero "audit actor" row, which is the wrong owner.
	c := &quotaCaller{}
	s := &Session{client: c}
	if _, err := s.GetQuota("user/x"); err == nil {
		t.Fatal("GetQuota on un-AUTH'd session must error")
	}
	if len(c.calls) != 0 {
		t.Errorf("calls: %v, want none (no RPC pre-auth)", c.calls)
	}
}

func TestSession_GetQuotaRoot_ReturnsAuthedLocalPart(t *testing.T) {
	s := &Session{
		actorID:         bytes32x(0xaa),
		authedLocalPart: "alice",
	}
	roots := s.GetQuotaRoot("INBOX")
	if len(roots) != 1 || roots[0] != "user/alice" {
		t.Errorf("GetQuotaRoot: %v, want [user/alice]", roots)
	}
}

func TestSession_GetQuotaRoot_FallsBackToHexPrefixWhenNoLocalPart(t *testing.T) {
	// When AUTH didn't capture a localPart (Phase F+ direct-keypair
	// AUTH path, for example), the quota root falls back to a
	// 16-hex-char actor_id prefix. Stable across sessions for the
	// same actor; recognizable in logs.
	actor := bytes32x(0xde)
	s := &Session{actorID: actor}
	roots := s.GetQuotaRoot("Drafts")
	if len(roots) != 1 {
		t.Fatalf("roots: %v", roots)
	}
	want := "user/dededededededede"[:21] // "user/" + 16 hex chars (0xde repeated)
	if roots[0] != want {
		t.Errorf("GetQuotaRoot: %q, want %q", roots[0], want)
	}
}

func TestSession_GetQuotaRoot_EmptyWhenNotAuthenticated(t *testing.T) {
	// RFC 9208 §3.2 — server MAY return an empty list when the
	// mailbox is outside any known quota root. A pre-AUTH session
	// has no quota root.
	s := &Session{}
	roots := s.GetQuotaRoot("INBOX")
	if len(roots) != 0 {
		t.Errorf("GetQuotaRoot pre-auth: %v, want []", roots)
	}
}
