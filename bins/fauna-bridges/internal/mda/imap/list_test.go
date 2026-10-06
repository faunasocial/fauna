package imap

import (
	"context"
	"errors"
	"log/slog"
	"strings"
	"testing"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// replyingCaller is a wsrpc.Caller that satisfies one method with a
// canned reply and rejects every other method. It mirrors the wsrpc
// package's recordingCaller (CBOR-roundtrip via dagcbor) so the
// wrapper sees a real wire shape regardless of its unexported reply
// type.
type replyingCaller struct {
	expectMethod string
	replyBody    any // typed payload that mirrors the wrapper's reply shape
	gotBody      []byte
	err          error
}

func (r *replyingCaller) Call(_ context.Context, method string, body, reply any) error {
	if r.err != nil {
		return r.err
	}
	if method != r.expectMethod {
		return errors.New("replyingCaller: unexpected method " + method + " (want " + r.expectMethod + ")")
	}
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	r.gotBody = enc
	if reply == nil || r.replyBody == nil {
		return nil
	}
	repBytes, err := dagcbor.Marshal(r.replyBody)
	if err != nil {
		return err
	}
	return cbor.Unmarshal(repBytes, reply)
}

// Session.List dispatches through the unexported listWriter seam in
// list.go (production wraps *imapserver.ListWriter; tests substitute
// the fakeListWriter at the bottom of this file). Spinning up a real
// *imapserver.ListWriter requires a *Conn and a bytes-pipe, which is
// 200 lines of plumbing for output we already trust emersion to
// produce.

func TestSessionListEmpty(t *testing.T) {
	c := &replyingCaller{
		expectMethod: "fauna.bridges.list_mailboxes",
		replyBody: struct {
			Mailboxes []wsrpc.MailboxEntry `cbor:"mailboxes"`
		}{Mailboxes: nil},
	}
	s := &Session{
		client:  c,
		logger:  slog.Default(),
		actorID: bytes32x(0x10),
	}
	w := &fakeListWriter{}
	if err := s.list(w, "", []string{"*"}, &imap.ListOptions{}); err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(w.entries) != 0 {
		t.Fatalf("expected 0 entries, got %d", len(w.entries))
	}
}

func TestSessionListPatternFiltersAndStandardAttrs(t *testing.T) {
	c := &replyingCaller{
		expectMethod: "fauna.bridges.list_mailboxes",
		replyBody: struct {
			Mailboxes []wsrpc.MailboxEntry `cbor:"mailboxes"`
		}{
			Mailboxes: []wsrpc.MailboxEntry{
				{Name: "INBOX"},
				{Name: "Drafts"},
				{Name: "Sent"},
				{Name: "Trash"},
				{Name: "Junk"},
				{Name: "Archive"},
				{Name: "Custom/Sub"},
			},
		},
	}
	s := &Session{
		client:  c,
		logger:  slog.Default(),
		actorID: bytes32x(0x11),
	}
	w := &fakeListWriter{}
	if err := s.list(w, "", []string{"*"}, &imap.ListOptions{}); err != nil {
		t.Fatalf("list: %v", err)
	}
	if got, want := len(w.entries), 7; got != want {
		t.Fatalf("entries: got %d, want %d", got, want)
	}
	want := map[string]imap.MailboxAttr{
		"Drafts":  imap.MailboxAttrDrafts,
		"Sent":    imap.MailboxAttrSent,
		"Trash":   imap.MailboxAttrTrash,
		"Junk":    imap.MailboxAttrJunk,
		"Archive": imap.MailboxAttrArchive,
	}
	for _, e := range w.entries {
		if e.Delim != '/' {
			t.Errorf("%s: delim %q, want /", e.Mailbox, e.Delim)
		}
		if exp, ok := want[e.Mailbox]; ok {
			found := false
			for _, a := range e.Attrs {
				if a == exp {
					found = true
					break
				}
			}
			if !found {
				t.Errorf("%s: missing attr %s; got %v", e.Mailbox, exp, e.Attrs)
			}
		}
		// INBOX is identified by name on the wire, no special-use attr
		// (per RFC 6154; emersion has no MailboxAttrInbox constant).
		if e.Mailbox == "INBOX" {
			for _, a := range e.Attrs {
				if strings.EqualFold(string(a), "\\Drafts") ||
					strings.EqualFold(string(a), "\\Sent") {
					t.Errorf("INBOX got special-use attr %s", a)
				}
			}
		}
	}
}

func TestSessionListAppliesPattern(t *testing.T) {
	c := &replyingCaller{
		expectMethod: "fauna.bridges.list_mailboxes",
		replyBody: struct {
			Mailboxes []wsrpc.MailboxEntry `cbor:"mailboxes"`
		}{
			Mailboxes: []wsrpc.MailboxEntry{
				{Name: "INBOX"},
				{Name: "Drafts"},
				{Name: "Sent"},
			},
		},
	}
	s := &Session{client: c, actorID: bytes32x(0x12)}
	w := &fakeListWriter{}
	if err := s.list(w, "", []string{"INBOX"}, &imap.ListOptions{}); err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(w.entries) != 1 || w.entries[0].Mailbox != "INBOX" {
		t.Fatalf("pattern filter: got %+v", w.entries)
	}
}

// ── D.7: SUBSCRIBED-axis tests ─────────────────────────────────────

// subscribedAwareCaller answers list_mailboxes with one fixture set
// when `subscribed_only=false` and a different (filtered) set when
// `subscribed_only=true`. Lets a single test fixture cover both the
// plain LIST and LSUB / RETURN (SUBSCRIBED) wire paths.
type subscribedAwareCaller struct {
	full       []wsrpc.MailboxEntry
	subscribed []wsrpc.MailboxEntry

	calls int
}

func (s *subscribedAwareCaller) Call(_ context.Context, method string, body, reply any) error {
	if method != "fauna.bridges.list_mailboxes" {
		return errors.New("subscribedAwareCaller: unexpected method " + method)
	}
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	var req struct {
		SubscribedOnly bool `cbor:"subscribed_only"`
	}
	if err := cbor.Unmarshal(enc, &req); err != nil {
		return err
	}
	s.calls++
	out := s.full
	if req.SubscribedOnly {
		out = s.subscribed
	}
	rep, err := dagcbor.Marshal(struct {
		Mailboxes []wsrpc.MailboxEntry `cbor:"mailboxes"`
	}{Mailboxes: out})
	if err != nil {
		return err
	}
	return cbor.Unmarshal(rep, reply)
}

func TestSessionListSubscribedOnlyForwardsTrueAndFiltersServerSide(t *testing.T) {
	c := &subscribedAwareCaller{
		full: []wsrpc.MailboxEntry{
			{Name: "INBOX"}, {Name: "Sent"}, {Name: "Drafts"},
		},
		subscribed: []wsrpc.MailboxEntry{
			{Name: "INBOX"}, {Name: "Sent"},
		},
	}
	s := &Session{client: c, logger: slog.Default(), actorID: bytes32x(0x21)}
	w := &fakeListWriter{}
	// LSUB / LIST (SUBSCRIBED) is `SelectSubscribed=true`.
	if err := s.list(w, "", []string{"*"}, &imap.ListOptions{SelectSubscribed: true}); err != nil {
		t.Fatalf("list: %v", err)
	}
	if c.calls != 1 {
		t.Errorf("expected 1 list_mailboxes RPC, got %d", c.calls)
	}
	got := names(w.entries)
	want := []string{"INBOX", "Sent"}
	if !equalStringSlice(got, want) {
		t.Fatalf("subscribed-only entries: got %v, want %v", got, want)
	}
	// Each returned mailbox must carry \Subscribed because the entire
	// reply set is by construction the subscription set.
	for _, e := range w.entries {
		if !hasAttr(e.Attrs, imap.MailboxAttrSubscribed) {
			t.Errorf("%s: missing \\Subscribed attr; got %v", e.Mailbox, e.Attrs)
		}
	}
}

func TestSessionListReturnSubscribedTagsOnlySubscribedMailboxes(t *testing.T) {
	c := &subscribedAwareCaller{
		full: []wsrpc.MailboxEntry{
			{Name: "INBOX"}, {Name: "Sent"}, {Name: "Drafts"}, {Name: "Junk"},
		},
		subscribed: []wsrpc.MailboxEntry{
			{Name: "INBOX"}, {Name: "Drafts"},
		},
	}
	s := &Session{client: c, logger: slog.Default(), actorID: bytes32x(0x22)}
	w := &fakeListWriter{}
	// LIST RETURN (SUBSCRIBED): the base set is the full mailbox set,
	// each subscribed one gets the \Subscribed attr.
	if err := s.list(w, "", []string{"*"}, &imap.ListOptions{ReturnSubscribed: true}); err != nil {
		t.Fatalf("list: %v", err)
	}
	if c.calls != 2 {
		t.Errorf("expected 2 list_mailboxes RPCs (base + subscribed), got %d", c.calls)
	}
	subscribed := map[string]bool{"INBOX": true, "Drafts": true}
	for _, e := range w.entries {
		hasSub := hasAttr(e.Attrs, imap.MailboxAttrSubscribed)
		want := subscribed[e.Mailbox]
		if hasSub != want {
			t.Errorf("%s: \\Subscribed=%v, want %v (attrs=%v)", e.Mailbox, hasSub, want, e.Attrs)
		}
	}
}

func TestSessionListPlainOmitsSubscribedAttrEntirely(t *testing.T) {
	// Plain LIST (no Select/Return SUBSCRIBED) issues exactly one RPC
	// and never tags \Subscribed regardless of actual subscription
	// state — RFC 9051 §6.3.9 (plain LIST does not surface
	// subscription state without RETURN options).
	c := &subscribedAwareCaller{
		full: []wsrpc.MailboxEntry{
			{Name: "INBOX"}, {Name: "Sent"},
		},
		subscribed: []wsrpc.MailboxEntry{
			{Name: "INBOX"},
		},
	}
	s := &Session{client: c, logger: slog.Default(), actorID: bytes32x(0x23)}
	w := &fakeListWriter{}
	if err := s.list(w, "", []string{"*"}, &imap.ListOptions{}); err != nil {
		t.Fatalf("list: %v", err)
	}
	if c.calls != 1 {
		t.Errorf("expected 1 list_mailboxes RPC, got %d", c.calls)
	}
	for _, e := range w.entries {
		if hasAttr(e.Attrs, imap.MailboxAttrSubscribed) {
			t.Errorf("plain LIST emitted \\Subscribed on %s: %v", e.Mailbox, e.Attrs)
		}
	}
}

func names(entries []*imap.ListData) []string {
	out := make([]string, 0, len(entries))
	for _, e := range entries {
		out = append(out, e.Mailbox)
	}
	return out
}

func equalStringSlice(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func hasAttr(attrs []imap.MailboxAttr, want imap.MailboxAttr) bool {
	for _, a := range attrs {
		if a == want {
			return true
		}
	}
	return false
}

// fakeListWriter implements the listWriter seam that Session.List
// dispatches through; production code wires the seam to
// *imapserver.ListWriter.WriteList.
type fakeListWriter struct {
	entries []*imap.ListData
}

func (f *fakeListWriter) WriteList(d *imap.ListData) error {
	f.entries = append(f.entries, d)
	return nil
}

// Compile-time check: fakeListWriter satisfies the listWriter seam.
var _ listWriter = (*fakeListWriter)(nil)

// bytes32x makes a 32-byte slice filled with v.
func bytes32x(v byte) []byte {
	out := make([]byte, 32)
	for i := range out {
		out[i] = v
	}
	return out
}
