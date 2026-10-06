package imap

import (
	"context"
	"errors"
	"testing"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// selectCaller stubs select_mailbox replies, plus the list_messages /
// fetch_message_metadata calls the inline SELECT (QRESYNC) fast-path
// issues to build its VANISHED (EARLIER) + changed-message FETCH set
// (RFC 7162 §3.2.5). `gotBody` captures the *select_mailbox* request
// body only (the later QRESYNC RPCs leave it untouched, so hint-
// forwarding assertions stay valid). listExpunged / metaRows are unset
// for plain SELECTs (the fast-path doesn't fire).
type selectCaller struct {
	gotMethod      string
	gotMbox        string
	gotBody        []byte
	gotSinceModseq *int64 // list_messages since_modseq, when the fast-path fires
	reply          map[string]any
	listExpunged   []uint32            // list_messages ExpungedUIDs → VANISHED (EARLIER)
	metaRows       []wsrpc.MessageMeta // fetch_message_metadata full ordered list
	err            error
}

func (s *selectCaller) Call(_ context.Context, method string, body, reply any) error {
	s.gotMethod = method
	if s.err != nil {
		return s.err
	}
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodSelectMailbox:
		s.gotBody = enc
		var bod struct {
			Mailbox string `cbor:"mailbox"`
		}
		_ = cbor.Unmarshal(enc, &bod)
		s.gotMbox = bod.Mailbox
		rep, err := dagcbor.Marshal(s.reply)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	case wsrpc.MethodListMessages:
		var bod struct {
			SinceModseq *int64 `cbor:"since_modseq"`
		}
		_ = cbor.Unmarshal(enc, &bod)
		s.gotSinceModseq = bod.SinceModseq
		rep, err := dagcbor.Marshal(wsrpc.ListMessagesReply{
			ExpungedUIDs:  s.listExpunged,
			HighestModseq: 100,
		})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	case wsrpc.MethodFetchMessageMetadata:
		rep, err := dagcbor.Marshal(struct {
			Messages []wsrpc.MessageMeta `cbor:"messages"`
		}{Messages: s.metaRows})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	default:
		return errors.New("selectCaller: unexpected " + method)
	}
}

func TestSessionSelectStashesState(t *testing.T) {
	first := uint32(2)
	c := &selectCaller{
		reply: map[string]any{
			"outcome":          "selected",
			"uid_validity":     uint32(7),
			"uid_next":         uint32(50),
			"highestmodseq":    int64(99),
			"exists":           uint32(20),
			"recent":           uint32(0),
			"unseen":           uint32(5),
			"first_unseen_uid": first,
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x33)}
	data, err := s.Select("INBOX", &imap.SelectOptions{})
	if err != nil {
		t.Fatalf("Select: %v", err)
	}
	if c.gotMbox != "INBOX" {
		t.Errorf("mailbox: %q", c.gotMbox)
	}
	if data.UIDValidity != 7 || data.UIDNext != 50 || data.NumMessages != 20 ||
		data.FirstUnseenSeqNum != 5 {
		// Note: nest's `unseen` is the *count* of unseen, not the
		// first-unseen sequence number; we map it onto NumMessages
		// minus exists-with-seen. The spec returns `first_unseen_uid`
		// (a UID) which the wire emits as OK [UNSEEN <seq>].
		// For the unit test we accept the count → FirstUnseenSeqNum
		// translation Phase C lands; Phase D refines.
		t.Errorf("data: %+v", data)
	}
	if data.HighestModSeq != 99 {
		t.Errorf("HighestModSeq: %d", data.HighestModSeq)
	}
	if s.selectedMailbox != "INBOX" {
		t.Errorf("selectedMailbox: %q", s.selectedMailbox)
	}
	if s.selectedUIDValidity != 7 {
		t.Errorf("selectedUIDValidity: %d", s.selectedUIDValidity)
	}
	if s.lastKnownModseq != 99 {
		t.Errorf("lastKnownModseq: %d", s.lastKnownModseq)
	}
	if len(data.Flags) == 0 {
		t.Errorf("Flags must include the standard system flags")
	}
}

func TestSessionSelectForwardsQResyncHint(t *testing.T) {
	c := &selectCaller{
		reply: map[string]any{
			"outcome":       "selected",
			"uid_validity":  uint32(7),
			"uid_next":      uint32(50),
			"highestmodseq": int64(99),
			"exists":        uint32(20),
			"recent":        uint32(0),
			"unseen":        uint32(0),
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x40)}
	// SELECT INBOX (QRESYNC (7 88)) — the fork's parser populates
	// options.QResync; the MDA must forward it as client_qresync so nest's
	// restore-divergence seam (γ) can compare last_modseq vs highestmodseq.
	_, err := s.Select("INBOX", &imap.SelectOptions{
		QResync: &imap.SelectQResync{UIDValidity: 7, ModSeq: 88},
	})
	if err != nil {
		t.Fatalf("Select: %v", err)
	}
	var body struct {
		ClientQresync *wsrpc.QResyncHint `cbor:"client_qresync"`
	}
	if err := cbor.Unmarshal(c.gotBody, &body); err != nil {
		t.Fatalf("decode body: %v", err)
	}
	if body.ClientQresync == nil {
		t.Fatalf("client_qresync must be forwarded when SELECT (QRESYNC ...) was supplied")
	}
	if body.ClientQresync.LastUIDValidity != 7 || body.ClientQresync.LastModseq != 88 {
		t.Errorf("client_qresync: %+v", body.ClientQresync)
	}
	if !s.qresyncEnabled {
		t.Errorf("qresyncEnabled must be set after SELECT (QRESYNC ...)")
	}
}

func TestSessionSelectOmitsQResyncHintForPlainSelect(t *testing.T) {
	c := &selectCaller{
		reply: map[string]any{
			"outcome":       "selected",
			"uid_validity":  uint32(1),
			"uid_next":      uint32(1),
			"highestmodseq": int64(1),
			"exists":        uint32(0),
			"recent":        uint32(0),
			"unseen":        uint32(0),
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x41)}
	if _, err := s.Select("INBOX", &imap.SelectOptions{}); err != nil {
		t.Fatalf("Select: %v", err)
	}
	var raw map[string]any
	if err := cbor.Unmarshal(c.gotBody, &raw); err != nil {
		t.Fatalf("decode body: %v", err)
	}
	if _, present := raw["client_qresync"]; present {
		t.Errorf("client_qresync must be absent for a plain SELECT")
	}
	if s.qresyncEnabled {
		t.Errorf("qresyncEnabled must stay false for a plain SELECT")
	}
}

func TestSessionSelectNoSuchMailboxIsErr(t *testing.T) {
	c := &selectCaller{
		reply: map[string]any{"outcome": "no_such_mailbox"},
	}
	s := &Session{client: c, actorID: bytes32x(0x34)}
	data, err := s.Select("Ghost", &imap.SelectOptions{})
	if err == nil {
		t.Fatalf("expected error, got %+v", data)
	}
	if data != nil {
		t.Errorf("data must be nil on error")
	}
	if s.selectedMailbox != "" {
		t.Errorf("state must remain unset after failed SELECT")
	}
}

func TestSessionSelectClearsPriorStateOnFail(t *testing.T) {
	// First SELECT succeeds, stashing state.
	first := uint32(1)
	c := &selectCaller{
		reply: map[string]any{
			"outcome":          "selected",
			"uid_validity":     uint32(1),
			"uid_next":         uint32(1),
			"highestmodseq":    int64(1),
			"exists":           uint32(0),
			"recent":           uint32(0),
			"unseen":           uint32(0),
			"first_unseen_uid": first,
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x35)}
	if _, err := s.Select("INBOX", &imap.SelectOptions{}); err != nil {
		t.Fatalf("first Select: %v", err)
	}
	if s.selectedMailbox != "INBOX" {
		t.Fatalf("first Select did not stash mailbox")
	}
	// Second SELECT misses — state must be cleared.
	c.reply = map[string]any{"outcome": "no_such_mailbox"}
	if _, err := s.Select("Ghost", &imap.SelectOptions{}); err == nil {
		t.Fatalf("expected error on missing mailbox")
	}
	if s.selectedMailbox != "" {
		t.Errorf("state must be cleared on failed SELECT, got %q", s.selectedMailbox)
	}
}

func TestSessionUnselectClearsState(t *testing.T) {
	s := &Session{
		selectedMailbox:     "INBOX",
		selectedUIDValidity: 7,
		lastKnownModseq:     99,
	}
	if err := s.Unselect(); err != nil {
		t.Fatalf("Unselect: %v", err)
	}
	if s.selectedMailbox != "" {
		t.Errorf("selectedMailbox: %q", s.selectedMailbox)
	}
	if s.selectedUIDValidity != 0 {
		t.Errorf("selectedUIDValidity: %d", s.selectedUIDValidity)
	}
	if s.lastKnownModseq != 0 {
		t.Errorf("lastKnownModseq: %d", s.lastKnownModseq)
	}
}

func TestSessionSelectExamineReadOnlyAccepted(t *testing.T) {
	first := uint32(1)
	c := &selectCaller{
		reply: map[string]any{
			"outcome":          "selected",
			"uid_validity":     uint32(1),
			"uid_next":         uint32(1),
			"highestmodseq":    int64(1),
			"exists":           uint32(0),
			"recent":           uint32(0),
			"unseen":           uint32(0),
			"first_unseen_uid": first,
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x36)}
	// EXAMINE in emersion is Select with options.ReadOnly=true.
	if _, err := s.Select("INBOX", &imap.SelectOptions{ReadOnly: true}); err != nil {
		t.Fatalf("EXAMINE: %v", err)
	}
}

func TestSessionSelectInlineQResyncEmitsVanishedAndChanged(t *testing.T) {
	// SELECT INBOX (QRESYNC (7 88)): uidvalidity matches (7) and
	// last_modseq 88 <= highestmodseq 99 — the common-case fast-path
	// (imap-server.md § QRESYNC SELECT). The MDA derives the inline
	// VANISHED (EARLIER) set from list_messages(since_modseq=88) and the
	// changed-message FETCH rows from the full metadata list, computing
	// each changed UID's sequence number from its position in that list.
	c := &selectCaller{
		reply: map[string]any{
			"outcome":       "selected",
			"uid_validity":  uint32(7),
			"uid_next":      uint32(20),
			"highestmodseq": int64(99),
			"exists":        uint32(3),
			"recent":        uint32(0),
			"unseen":        uint32(0),
		},
		listExpunged: []uint32{7, 9},
		metaRows: []wsrpc.MessageMeta{
			{UID: 1, MessageID: bytes32x(0x01), Flags: []string{}, Modseq: 50, InternalDate: 1, CiphertextSize: 1},
			{UID: 5, MessageID: bytes32x(0x05), Flags: []string{"\\Seen"}, Modseq: 95, InternalDate: 1, CiphertextSize: 1},
			{UID: 12, MessageID: bytes32x(0x0c), Flags: []string{"\\Flagged"}, Modseq: 99, InternalDate: 1, CiphertextSize: 1},
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x50)}
	data, err := s.Select("INBOX", &imap.SelectOptions{
		QResync: &imap.SelectQResync{UIDValidity: 7, ModSeq: 88},
	})
	if err != nil {
		t.Fatalf("Select: %v", err)
	}
	if data.QResync == nil {
		t.Fatalf("data.QResync must be populated for the common-case inline fast-path")
	}
	// list_messages queried with since_modseq = the client's last_modseq.
	if c.gotSinceModseq == nil || *c.gotSinceModseq != 88 {
		t.Errorf("list_messages since_modseq = %v, want 88", c.gotSinceModseq)
	}
	// VANISHED (EARLIER) is the expunge-log set since last_modseq.
	if got := data.QResync.Vanished.String(); got != "7,9" {
		t.Errorf("Vanished = %q, want 7,9", got)
	}
	// Changed = rows with modseq > 88, at their current sequence numbers.
	// UID 1 (modseq 50) is unchanged and skipped; UID 5 → seq 2; UID 12 → seq 3.
	if len(data.QResync.Changed) != 2 {
		t.Fatalf("Changed = %d entries, want 2", len(data.QResync.Changed))
	}
	c0, c1 := data.QResync.Changed[0], data.QResync.Changed[1]
	if c0.SeqNum != 2 || c0.UID != 5 || c0.ModSeq != 95 {
		t.Errorf("Changed[0] = %+v, want {seq 2, uid 5, modseq 95}", c0)
	}
	if len(c0.Flags) != 1 || c0.Flags[0] != imap.FlagSeen {
		t.Errorf("Changed[0].Flags = %v, want [\\Seen]", c0.Flags)
	}
	if c1.SeqNum != 3 || c1.UID != 12 || c1.ModSeq != 99 {
		t.Errorf("Changed[1] = %+v, want {seq 3, uid 12, modseq 99}", c1)
	}
}

func TestSessionSelectInlineQResyncSkipsWhenModseqAheadOfServer(t *testing.T) {
	// last_modseq (150) > highestmodseq (99): the restore-divergence (γ)
	// case (imap-server.md § Restore divergence detection). Nest writes a
	// divergence row and the client falls through to a full resync; the
	// MDA must NOT synthesize an inline fast-path, and must not waste the
	// list_messages / fetch_message_metadata round-trips.
	c := &selectCaller{
		reply: map[string]any{
			"outcome":       "selected",
			"uid_validity":  uint32(7),
			"uid_next":      uint32(20),
			"highestmodseq": int64(99),
			"exists":        uint32(1),
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x51)}
	data, err := s.Select("INBOX", &imap.SelectOptions{
		QResync: &imap.SelectQResync{UIDValidity: 7, ModSeq: 150},
	})
	if err != nil {
		t.Fatalf("Select: %v", err)
	}
	if data.QResync != nil {
		t.Errorf("data.QResync must be nil when last_modseq exceeds highestmodseq")
	}
	if c.gotSinceModseq != nil {
		t.Errorf("list_messages must not be queried in the divergence case")
	}
}

func TestSessionSelectInlineQResyncSkipsOnUIDValidityChange(t *testing.T) {
	// last_uid_validity (3) != server uid_validity (7): the mailbox was
	// deleted-and-recreated, so QRESYNC cannot fast-path (imap-server.md
	// § QRESYNC SELECT — stale UIDVALIDITY → full resync). No inline output.
	c := &selectCaller{
		reply: map[string]any{
			"outcome":       "selected",
			"uid_validity":  uint32(7),
			"uid_next":      uint32(20),
			"highestmodseq": int64(99),
			"exists":        uint32(1),
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x52)}
	data, err := s.Select("INBOX", &imap.SelectOptions{
		QResync: &imap.SelectQResync{UIDValidity: 3, ModSeq: 50},
	})
	if err != nil {
		t.Fatalf("Select: %v", err)
	}
	if data.QResync != nil {
		t.Errorf("data.QResync must be nil when UIDVALIDITY changed")
	}
	if c.gotSinceModseq != nil {
		t.Errorf("list_messages must not be queried on UIDVALIDITY change")
	}
}
