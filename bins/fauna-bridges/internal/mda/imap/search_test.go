package imap

import (
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"sort"
	"testing"
	"time"
	"unicode/utf8"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// ── partitionSearchCriteria tests ─────────────────────────────────

func TestPartitionSearchCriteria_FlagsHeadersDatesSizesSplitCorrectly(t *testing.T) {
	since := time.Date(2024, 1, 1, 12, 0, 0, 0, time.UTC)
	before := time.Date(2024, 2, 1, 18, 30, 0, 0, time.UTC)

	c := &imap.SearchCriteria{
		Flag:    []imap.Flag{`\Seen`, `keyword-foo`},
		NotFlag: []imap.Flag{`\Deleted`},
		Header: []imap.SearchCriteriaHeaderField{
			{Key: "From", Value: "Example.COM"},
			{Key: "Subject", Value: "Invoice"},
		},
		Since:   since,
		Before:  before,
		Larger:  1024,
		Smaller: 1_000_000,
	}

	out, err := partitionSearchCriteria(c)
	if err != nil {
		t.Fatalf("partition: %v", err)
	}

	if len(out.bodyTerms) != 0 {
		t.Errorf("bodyTerms: got %v, want empty", out.bodyTerms)
	}

	// Expect 2 flag terms + 1 lacks-flag + 2 header terms + 2 date terms +
	// 1 larger + 1 smaller = 9.
	if len(out.flagTerms) != 9 {
		t.Fatalf("flagTerms count: %d, want 9; terms=%+v", len(out.flagTerms), out.flagTerms)
	}

	want := []wsrpc.SearchTermKind{
		wsrpc.SearchTermKindHasFlag,
		wsrpc.SearchTermKindHasFlag,
		wsrpc.SearchTermKindLacksFlag,
		wsrpc.SearchTermKindHeaderContains,
		wsrpc.SearchTermKindHeaderContains,
		wsrpc.SearchTermKindSinceInternalDate,
		wsrpc.SearchTermKindBeforeInternalDate,
		wsrpc.SearchTermKindLarger,
		wsrpc.SearchTermKindSmaller,
	}
	for i, w := range want {
		if out.flagTerms[i].Kind != w {
			t.Errorf("flagTerms[%d].Kind = %q, want %q", i, out.flagTerms[i].Kind, w)
		}
	}

	if out.flagTerms[3].Value != "example.com" {
		t.Errorf("header From value: %q, want lowercased", out.flagTerms[3].Value)
	}
	if out.flagTerms[3].Field != wsrpc.HeaderFieldFrom {
		t.Errorf("header field: %q, want from", out.flagTerms[3].Field)
	}

	if out.flagTerms[5].Ts == 0 {
		t.Errorf("since ts unset")
	}
	if out.flagTerms[6].Ts == 0 {
		t.Errorf("before ts unset")
	}

	if out.flagTerms[7].Size != 1024 {
		t.Errorf("larger size: %d", out.flagTerms[7].Size)
	}
	if out.flagTerms[8].Size != 1_000_000 {
		t.Errorf("smaller size: %d", out.flagTerms[8].Size)
	}
}

func TestPartitionSearchCriteria_BodyAndTextBecomeBodyTerms(t *testing.T) {
	c := &imap.SearchCriteria{
		Body: []string{"invoice", "Q4"},
		Text: []string{"NewSletter"},
	}
	out, err := partitionSearchCriteria(c)
	if err != nil {
		t.Fatalf("partition: %v", err)
	}
	if len(out.flagTerms) != 0 {
		t.Errorf("flagTerms: got %d, want 0", len(out.flagTerms))
	}
	sort.Strings(out.bodyTerms)
	want := []string{"invoice", "newsletter", "q4"}
	if len(out.bodyTerms) != len(want) {
		t.Fatalf("bodyTerms: %v, want %v", out.bodyTerms, want)
	}
	for i, w := range want {
		if out.bodyTerms[i] != w {
			t.Errorf("bodyTerms[%d]: %q, want %q", i, out.bodyTerms[i], w)
		}
	}
}

func TestPartitionSearchCriteria_RejectsOrAndNot(t *testing.T) {
	c := &imap.SearchCriteria{Not: []imap.SearchCriteria{{Body: []string{"x"}}}}
	if _, err := partitionSearchCriteria(c); err == nil {
		t.Errorf("expected error for Not, got nil")
	}
	c = &imap.SearchCriteria{Or: [][2]imap.SearchCriteria{{
		{Body: []string{"a"}}, {Body: []string{"b"}},
	}}}
	if _, err := partitionSearchCriteria(c); err == nil {
		t.Errorf("expected error for Or, got nil")
	}
}

func TestPartitionSearchCriteria_RejectsUnsupportedHeaderKey(t *testing.T) {
	c := &imap.SearchCriteria{
		Header: []imap.SearchCriteriaHeaderField{{Key: "Date", Value: "2024"}},
	}
	if _, err := partitionSearchCriteria(c); err == nil {
		t.Errorf("expected error for unsupported header Key=Date")
	}
}

func TestPartitionSearchCriteria_RejectsSentSinceSentBefore(t *testing.T) {
	c := &imap.SearchCriteria{SentSince: time.Now()}
	if _, err := partitionSearchCriteria(c); err == nil {
		t.Errorf("expected error for SentSince")
	}
	c = &imap.SearchCriteria{SentBefore: time.Now()}
	if _, err := partitionSearchCriteria(c); err == nil {
		t.Errorf("expected error for SentBefore")
	}
}

// ── bodySearch tests ──────────────────────────────────────────────
//
// bodySearch opens every hint through `mailfauna.OpenStoredRecordAt`: a hint
// rests sealed, so the fixtures seal theirs (fixtureSeal) and pass the real
// fixtureOpener.

func TestBodySearch_MatchesTokenizedSegmentsByUID(t *testing.T) {
	mid1 := [32]byte{0x11}
	mid2 := [32]byte{0x22}
	mid3 := [32]byte{0x33}
	uidByMid := map[string]uint32{
		string(mid1[:]): 5,
		string(mid2[:]): 7,
		string(mid3[:]): 9,
	}

	segs := []wsrpc.IndexSegment{
		{MessageID: mid1[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: fixtureSeal([]byte("Hello there friend"))},
		{MessageID: mid2[:], Mailbox: "INBOX", Modseq: 2, EncryptedIndexHint: fixtureSeal([]byte("invoice quarterly summary"))},
		{MessageID: mid3[:], Mailbox: "INBOX", Modseq: 3, EncryptedIndexHint: fixtureSeal([]byte("hello newsletter weekly"))},
	}

	got, err := bodySearch(fixtureOpener(), segs, []string{"hello"}, uidByMid, nil)
	if err != nil {
		t.Fatalf("bodySearch: %v", err)
	}
	sort.Slice(got, func(i, j int) bool { return got[i] < got[j] })
	want := []uint32{5, 9}
	if len(got) != len(want) || got[0] != want[0] || got[1] != want[1] {
		t.Errorf("hello match: got %v, want %v", got, want)
	}

	// Only the opened plaintext is wiped after tokenizing; the sealed hints
	// are reusable for the follow-up matches.
	got, err = bodySearch(fixtureOpener(), segs, []string{"hello", "newsletter"}, uidByMid, nil)
	if err != nil {
		t.Fatalf("bodySearch: %v", err)
	}
	if len(got) != 1 || got[0] != 9 {
		t.Errorf("hello+newsletter: %v, want [9]", got)
	}

	got, _ = bodySearch(fixtureOpener(), segs, []string{"absent"}, uidByMid, nil)
	if len(got) != 0 {
		t.Errorf("absent: %v, want []", got)
	}
}

func TestBodySearch_SkipsSegmentsWhoseUIDIsUnknown(t *testing.T) {
	mid := [32]byte{0xff}
	segs := []wsrpc.IndexSegment{
		{MessageID: mid[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: fixtureSeal([]byte("hello"))},
	}
	got, err := bodySearch(fixtureOpener(), segs, []string{"hello"}, map[string]uint32{}, nil)
	if err != nil {
		t.Fatalf("bodySearch: %v", err)
	}
	if len(got) != 0 {
		t.Errorf("expected empty result for unknown UID, got %v", got)
	}
}

// TestBodySearch_SealedHintOpensViaRealOpener pins the sealed arm of the
// per-record rule: a hint sealed to the actor's leaf key (the production
// APPEND/MTA shape) HPKE-opens through a REAL per-connection opener and
// tokenizes to the sealed plaintext — identically in both storage modes
// (there is no mode input to reach for).
func TestBodySearch_SealedHintOpensViaRealOpener(t *testing.T) {
	opener, seal := realOpenerFixture(t)
	mid1 := [32]byte{0x11}
	mid2 := [32]byte{0x22}
	uidByMid := map[string]uint32{
		string(mid1[:]): 5,
		string(mid2[:]): 7,
	}
	segs := []wsrpc.IndexSegment{
		{MessageID: mid1[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: seal([]byte("hello there friend"))},
		{MessageID: mid2[:], Mailbox: "INBOX", Modseq: 2, EncryptedIndexHint: seal([]byte("invoice quarterly summary"))},
	}
	got, err := bodySearch(opener, segs, []string{"hello"}, uidByMid, nil)
	if err != nil {
		t.Fatalf("bodySearch (sealed): %v", err)
	}
	if len(got) != 1 || got[0] != 5 {
		t.Errorf("sealed hello match: got %v, want [5]", got)
	}
}

// TestBodySearch_SealedHintWithoutOpenerErrors: a sealed hint on a session
// with no opener (no MLS snapshot provisioned) must error per hint — never
// tokenize the ciphertext as if it were the plaintext.
func TestBodySearch_SealedHintWithoutOpenerErrors(t *testing.T) {
	mid := [32]byte{0x11}
	uidByMid := map[string]uint32{string(mid[:]): 5}
	segs := []wsrpc.IndexSegment{
		{MessageID: mid[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: sealedEnvelopeFixture(t, []byte("hello"))},
	}
	if _, err := bodySearch(nil, segs, []string{"hello"}, uidByMid, nil); err == nil {
		t.Fatalf("sealed hint with no opener must error, got nil")
	}
}

// ── Session.Search integration tests ─────────────────────────────

type searchCaller struct {
	calls           []string
	searchReplyUIDs []uint32
	metadataReply   []wsrpc.MessageMeta
	segmentsReply   []wsrpc.IndexSegment
}

func (s *searchCaller) Call(_ context.Context, method string, body, reply any) error {
	s.calls = append(s.calls, method)
	if _, err := dagcbor.Marshal(body); err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodSearchMessages:
		rep, err := dagcbor.Marshal(struct {
			UIDs []uint32 `cbor:"uids"`
		}{UIDs: s.searchReplyUIDs})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	case wsrpc.MethodFetchMessageMetadata:
		rep, err := dagcbor.Marshal(struct {
			Messages []wsrpc.MessageMeta `cbor:"messages"`
		}{Messages: s.metadataReply})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	case wsrpc.MethodFetchIndexSegmentsSince:
		rep, err := dagcbor.Marshal(wsrpc.FetchIndexSegmentsSinceReply{
			Segments:      s.segmentsReply,
			HighestModseq: 1,
			More:          false,
		})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	}
	return errors.New("searchCaller: unexpected method " + method)
}

func TestSession_Search_FlagsOnlyIssuesOneRPC(t *testing.T) {
	c := &searchCaller{searchReplyUIDs: []uint32{2, 5, 9}}
	s := &Session{
		client:          c,
		actorID:         bytes32x(0xaa),
		selectedMailbox: "INBOX",
	}

	data, err := s.searchWithDecryptor(imapserver.NumKindUID, &imap.SearchCriteria{
		Flag: []imap.Flag{`\Seen`},
	}, &imap.SearchOptions{}, nil)
	if err != nil {
		t.Fatalf("Search: %v", err)
	}
	if len(c.calls) != 1 || c.calls[0] != wsrpc.MethodSearchMessages {
		t.Errorf("calls: %v, want only [search_messages]", c.calls)
	}
	uids := data.AllUIDs()
	if len(uids) != 3 || uint32(uids[0]) != 2 || uint32(uids[2]) != 9 {
		t.Errorf("uids: %v", uids)
	}
}

func TestSession_Search_BodyOnlyFetchesIndexSegmentsAndFilters(t *testing.T) {
	mid1 := [32]byte{0x44}
	mid2 := [32]byte{0x55}
	c := &searchCaller{
		metadataReply: []wsrpc.MessageMeta{
			{UID: 3, MessageID: mid1[:], Flags: []string{}, InternalDate: 1, CiphertextSize: 1},
			{UID: 4, MessageID: mid2[:], Flags: []string{}, InternalDate: 2, CiphertextSize: 1},
		},
		segmentsReply: []wsrpc.IndexSegment{
			{MessageID: mid1[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: fixtureSeal([]byte("invoice"))},
			{MessageID: mid2[:], Mailbox: "INBOX", Modseq: 2, EncryptedIndexHint: fixtureSeal([]byte("newsletter"))},
		},
	}
	s := &Session{
		client:          c,
		actorID:         bytes32x(0xbb),
		selectedMailbox: "INBOX",
	}

	data, err := s.searchWithDecryptor(imapserver.NumKindUID, &imap.SearchCriteria{
		Body: []string{"invoice"},
	}, &imap.SearchOptions{}, fixtureOpener())
	if err != nil {
		t.Fatalf("Search: %v", err)
	}
	if len(c.calls) != 2 {
		t.Fatalf("calls: %v", c.calls)
	}
	gotKinds := map[string]bool{}
	for _, m := range c.calls {
		gotKinds[m] = true
	}
	if !gotKinds[wsrpc.MethodFetchMessageMetadata] ||
		!gotKinds[wsrpc.MethodFetchIndexSegmentsSince] {
		t.Errorf("expected metadata + index segments, got %v", c.calls)
	}
	uids := data.AllUIDs()
	if len(uids) != 1 || uint32(uids[0]) != 3 {
		t.Errorf("uids: %v, want [3]", uids)
	}
}

func TestSession_Search_MixedFlagsAndBodyIntersects(t *testing.T) {
	mid1 := [32]byte{0x66}
	mid2 := [32]byte{0x77}
	c := &searchCaller{
		searchReplyUIDs: []uint32{3, 4},
		metadataReply: []wsrpc.MessageMeta{
			{UID: 3, MessageID: mid1[:]},
			{UID: 4, MessageID: mid2[:]},
		},
		segmentsReply: []wsrpc.IndexSegment{
			{MessageID: mid1[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: fixtureSeal([]byte("hello"))},
			{MessageID: mid2[:], Mailbox: "INBOX", Modseq: 2, EncryptedIndexHint: fixtureSeal([]byte("invoice"))},
		},
	}
	s := &Session{
		client:          c,
		actorID:         bytes32x(0xcc),
		selectedMailbox: "INBOX",
	}

	data, err := s.searchWithDecryptor(imapserver.NumKindUID, &imap.SearchCriteria{
		Flag: []imap.Flag{`\Seen`},
		Body: []string{"invoice"},
	}, &imap.SearchOptions{}, fixtureOpener())
	if err != nil {
		t.Fatalf("Search: %v", err)
	}
	uids := data.AllUIDs()
	if len(uids) != 1 || uint32(uids[0]) != 4 {
		t.Errorf("intersection: got %v, want [4]", uids)
	}
}

func TestSession_Search_RequiresSelectedMailbox(t *testing.T) {
	s := &Session{
		client:  &searchCaller{},
		actorID: bytes32x(0xdd),
	}
	_, err := s.searchWithDecryptor(imapserver.NumKindUID, &imap.SearchCriteria{}, &imap.SearchOptions{}, nil)
	if err == nil {
		t.Errorf("expected error for no SELECT")
	}
}

func TestSession_Search_RejectsOrAndNot(t *testing.T) {
	s := &Session{
		client:          &searchCaller{},
		actorID:         bytes32x(0xee),
		selectedMailbox: "INBOX",
	}

	_, err := s.searchWithDecryptor(imapserver.NumKindUID, &imap.SearchCriteria{
		Not: []imap.SearchCriteria{{Body: []string{"x"}}},
	}, &imap.SearchOptions{}, nil)
	if err == nil {
		t.Errorf("expected error for Not")
	}
}

// TestSession_Search_SealedHintWithoutOpenerErrors replaces the old upfront
// "body-axis SEARCH requires unwrapped MLS" gate test: since Phase-3 S2 the
// missing-opener error surfaces PER HINT, when the first hint is opened.
func TestSession_Search_SealedHintWithoutOpenerErrors(t *testing.T) {
	mid := [32]byte{0x88}
	c := &searchCaller{
		metadataReply: []wsrpc.MessageMeta{
			{UID: 3, MessageID: mid[:], Flags: []string{}, InternalDate: 1, CiphertextSize: 1},
		},
		segmentsReply: []wsrpc.IndexSegment{
			{MessageID: mid[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: sealedEnvelopeFixture(t, []byte("hello"))},
		},
	}
	s := &Session{
		client:          c,
		actorID:         bytes32x(0xff),
		selectedMailbox: "INBOX",
	}
	_, err := s.searchWithDecryptor(imapserver.NumKindUID, &imap.SearchCriteria{
		Body: []string{"hello"},
	}, &imap.SearchOptions{}, nil)
	if err == nil {
		t.Errorf("expected error: sealed index hint with no opener on file")
	}
}

// ── the content-index hybrid ─────────────────────────────────────────────
//
// The body axis answers from the sealed slice where it covers a message and
// scans where it does not. These pin the split, because both ways of getting it
// wrong are silent: trusting an incomplete slice loses mail from the reply, and
// distrusting a complete one just pays the decrypt twice.

// answerStub is an indexAnswerFunc over fixed sets, recording what it was asked.
//
// It returns the sets **alongside** any error, deliberately. A stub that
// answered `nil, nil, err` would make the caller's error check unfalsifiable —
// discarding empty sets and honouring them are the same thing — and the failure
// mode worth pinning is precisely a partial answer arriving with an error.
func answerStub(covered, matched []string, err error) (indexAnswerFunc, *[]string) {
	asked := new([]string)
	return func(terms []string, candidates []string) (map[string]bool, map[string]bool, error) {
		*asked = append(*asked, candidates...)
		c := map[string]bool{}
		for _, m := range covered {
			c[m] = true
		}
		m := map[string]bool{}
		for _, x := range matched {
			m[x] = true
		}
		return c, m, err
	}, asked
}

// A covered message takes its verdict from the index and its hint is never
// opened. The hint text here deliberately does NOT contain the term, so a
// scan could not produce this answer — the UID's presence proves the index
// was the source, and the untouched stager proves the HPKE open was skipped.
func TestBodySearch_CoveredMessageIsAnsweredFromTheIndexWithoutOpeningIt(t *testing.T) {
	mid := [32]byte{0x11}
	uidByMid := map[string]uint32{string(mid[:]): 5}
	segs := []wsrpc.IndexSegment{
		{MessageID: mid[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: fixtureSeal([]byte("nothing relevant here"))},
	}
	answer, asked := answerStub([]string{string(mid[:])}, []string{string(mid[:])}, nil)

	got, err := bodySearch(fixtureOpener(), segs, []string{"invoice"}, uidByMid, answer)
	if err != nil {
		t.Fatalf("bodySearch: %v", err)
	}
	if len(got) != 1 || got[0] != 5 {
		t.Fatalf("got %v, want [5] — the index's verdict, which the scan could not have produced", got)
	}
	if len(*asked) != 1 || (*asked)[0] != string(mid[:]) {
		t.Errorf("asked the index about %v, want exactly the one resolvable candidate", *asked)
	}
}

// Covered-but-not-matching is an answer, not a reason to rescan: the hint says
// "invoice" but the index says no, and the index wins for messages it covers.
func TestBodySearch_CoveredNonMatchIsNotRescanned(t *testing.T) {
	mid := [32]byte{0x22}
	uidByMid := map[string]uint32{string(mid[:]): 7}
	segs := []wsrpc.IndexSegment{
		{MessageID: mid[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: fixtureSeal([]byte("invoice quarterly"))},
	}
	answer, _ := answerStub([]string{string(mid[:])}, nil, nil)

	got, err := bodySearch(fixtureOpener(), segs, []string{"invoice"}, uidByMid, answer)
	if err != nil {
		t.Fatalf("bodySearch: %v", err)
	}
	if len(got) != 0 {
		t.Errorf("got %v, want [] — the index answered NO for a message it covers; the hint says invoice, so a rescan would have re-matched it", got)
	}
}

// The mixed mailbox, which is the normal state: the slice holds what earlier
// searches decrypted, so one message is answered and the other is scanned.
func TestBodySearch_UncoveredMessagesAreStillScanned(t *testing.T) {
	indexed := [32]byte{0x11}
	fresh := [32]byte{0x22}
	uidByMid := map[string]uint32{string(indexed[:]): 5, string(fresh[:]): 7}
	segs := []wsrpc.IndexSegment{
		{MessageID: indexed[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: fixtureSeal([]byte("nothing relevant"))},
		{MessageID: fresh[:], Mailbox: "INBOX", Modseq: 2, EncryptedIndexHint: fixtureSeal([]byte("invoice quarterly"))},
	}
	answer, _ := answerStub([]string{string(indexed[:])}, []string{string(indexed[:])}, nil)

	got, err := bodySearch(fixtureOpener(), segs, []string{"invoice"}, uidByMid, answer)
	if err != nil {
		t.Fatalf("bodySearch: %v", err)
	}
	sort.Slice(got, func(i, j int) bool { return got[i] < got[j] })
	if len(got) != 2 || got[0] != 5 || got[1] != 7 {
		t.Fatalf("got %v, want [5 7] — one from the index (its hint could not match), one from the scan (the index never covered it)", got)
	}
}

// **The fallback.** An index failure must degrade to the
// pre-index scan bit for bit — never a partial answer, and never an error the
// user's MUA sees. A rail outage costs latency, not results.
func TestBodySearch_IndexFailureFallsBackToTheFullScan(t *testing.T) {
	mid1 := [32]byte{0x11}
	mid2 := [32]byte{0x22}
	uidByMid := map[string]uint32{string(mid1[:]): 5, string(mid2[:]): 7}
	mk := func() []wsrpc.IndexSegment {
		return []wsrpc.IndexSegment{
			{MessageID: mid1[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: fixtureSeal([]byte("invoice quarterly"))},
			{MessageID: mid2[:], Mailbox: "INBOX", Modseq: 2, EncryptedIndexHint: fixtureSeal([]byte("lunch plans"))},
		}
	}
	// A PARTIAL answer arriving with an error — the shape that makes the error
	// check falsifiable. Honouring these sets would mark the one real match
	// covered-but-not-matching and drop it from the reply.
	failing, _ := answerStub([]string{string(mid1[:])}, nil, errors.New("nest unreachable"))

	withFailure, err := bodySearch(fixtureOpener(), mk(), []string{"invoice"}, uidByMid, failing)
	if err != nil {
		t.Fatalf("an index outage must not fail the SEARCH: %v", err)
	}
	withoutIndex, err := bodySearch(fixtureOpener(), mk(), []string{"invoice"}, uidByMid, nil)
	if err != nil {
		t.Fatalf("bodySearch (no index): %v", err)
	}
	if len(withFailure) != len(withoutIndex) || len(withFailure) != 1 || withFailure[0] != withoutIndex[0] {
		t.Errorf("index failure changed the answer: %v vs %v", withFailure, withoutIndex)
	}
}

// **Parity.** One corpus, both paths, same answer — the proof row 30 asks for
// before the swap is allowed to serve anything. The index is given the verdicts
// a correct index would hold for this corpus, and must reproduce the scan.
func TestBodySearch_IndexAndScanAgreeOnTheSameCorpus(t *testing.T) {
	mids := [][32]byte{{0x11}, {0x22}, {0x33}, {0x44}}
	bodies := []string{
		"alpha and beta together",
		"alpha only here",
		"beta only here",
		"neither of them",
	}
	uidByMid := map[string]uint32{}
	for i, m := range mids {
		uidByMid[string(m[:])] = uint32(10 + i)
	}
	mk := func() []wsrpc.IndexSegment {
		segs := make([]wsrpc.IndexSegment, len(mids))
		for i := range mids {
			segs[i] = wsrpc.IndexSegment{
				MessageID: mids[i][:], Mailbox: "INBOX", Modseq: int64(i + 1),
				EncryptedIndexHint: fixtureSeal([]byte(bodies[i])),
			}
		}
		return segs
	}

	for _, tc := range []struct {
		name    string
		terms   []string
		matched []string
	}{
		{"single term", []string{"alpha"}, []string{string(mids[0][:]), string(mids[1][:])}},
		{"two terms AND", []string{"alpha", "beta"}, []string{string(mids[0][:])}},
		{"multi-token term", []string{"alpha beta"}, []string{string(mids[0][:])}},
		{"no matches", []string{"absent"}, nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			all := make([]string, 0, len(mids))
			for _, m := range mids {
				all = append(all, string(m[:]))
			}
			// Everything covered: the index alone answers, and must agree.
			answer, _ := answerStub(all, tc.matched, nil)
			fromIndex, err := bodySearch(fixtureOpener(), mk(), tc.terms, uidByMid, answer)
			if err != nil {
				t.Fatalf("index path: %v", err)
			}
			fromScan, err := bodySearch(fixtureOpener(), mk(), tc.terms, uidByMid, nil)
			if err != nil {
				t.Fatalf("scan path: %v", err)
			}
			sort.Slice(fromIndex, func(i, j int) bool { return fromIndex[i] < fromIndex[j] })
			sort.Slice(fromScan, func(i, j int) bool { return fromScan[i] < fromScan[j] })
			if fmt.Sprint(fromIndex) != fmt.Sprint(fromScan) {
				t.Errorf("paths disagree: index %v, scan %v", fromIndex, fromScan)
			}
		})
	}
}

// Two placements of one Message-ID must get ONE verdict — asked about once, and
// both UIDs answered alike. A half-scanned/half-indexed split would report the
// same message inconsistently depending on placement order.
func TestBodySearch_ADuplicateMessageIDGetsOneVerdict(t *testing.T) {
	mid := [32]byte{0x11}
	uidByMid := map[string]uint32{string(mid[:]): 5}
	segs := []wsrpc.IndexSegment{
		{MessageID: mid[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: fixtureSeal([]byte("nothing relevant"))},
		{MessageID: mid[:], Mailbox: "INBOX", Modseq: 2, EncryptedIndexHint: fixtureSeal([]byte("nothing relevant"))},
	}
	answer, asked := answerStub([]string{string(mid[:])}, []string{string(mid[:])}, nil)

	got, err := bodySearch(fixtureOpener(), segs, []string{"invoice"}, uidByMid, answer)
	if err != nil {
		t.Fatalf("bodySearch: %v", err)
	}
	if len(*asked) != 1 {
		t.Errorf("asked about %d candidates, want 1 — a duplicate mid is one question", len(*asked))
	}
	if len(got) != 2 || got[0] != 5 || got[1] != 5 {
		t.Errorf("got %v, want both placements answered alike", got)
	}
}

// Terms that tokenize to nothing impose no constraint, so the scan returns
// everything — and the index is not consulted at all, since there is nothing
// for it to narrow.
func TestBodySearch_UntokenizableTermsSkipTheIndexEntirely(t *testing.T) {
	mid := [32]byte{0x11}
	uidByMid := map[string]uint32{string(mid[:]): 5}
	segs := []wsrpc.IndexSegment{
		{MessageID: mid[:], Mailbox: "INBOX", Modseq: 1, EncryptedIndexHint: fixtureSeal([]byte("anything"))},
	}
	answer, asked := answerStub(nil, nil, errors.New("must not be called"))

	got, err := bodySearch(fixtureOpener(), segs, []string{"!"}, uidByMid, answer)
	if err != nil {
		t.Fatalf("bodySearch: %v", err)
	}
	if len(*asked) != 0 {
		t.Errorf("index consulted for an unconstrained search: %v", *asked)
	}
	if len(got) != 1 || got[0] != 5 {
		t.Errorf("got %v, want [5] — no tokens means no constraint", got)
	}
}

// **The defect the tier_3 inbound test caught, pinned where it is cheap.**
// `wsrpc.IndexSegment.MessageID` is a raw 32-byte nest id, and every id-shaped
// field crossing the UniFFI boundary is a String whose Go→Rust converter
// PANICS on invalid UTF-8 — so `string(seg.MessageID)` killed the IMAP
// connection on the first real message. A Go string holds arbitrary bytes
// happily, which is exactly why no Go test noticed until a real FFI call ran.
func TestIndexDocID_EncodesBinaryMessageIDsAsValidUTF8(t *testing.T) {
	// A real nest message id: 32 bytes, first byte invalid UTF-8 on its own.
	raw := make([]byte, 32)
	for i := range raw {
		raw[i] = byte(0x80 + i)
	}
	if utf8.ValidString(string(raw)) {
		t.Fatal("fixture must be invalid UTF-8, else it cannot reproduce the panic")
	}

	got := indexDocID(raw)
	if !utf8.ValidString(got) {
		t.Errorf("indexDocID must produce a String the FFI can carry; got %q", got)
	}
	if len(got) != 2*len(raw) {
		t.Errorf("hex of %d bytes should be %d chars; got %d", len(raw), 2*len(raw), len(got))
	}

	// Injective, so two distinct messages can never collide into one doc.
	other := append([]byte(nil), raw...)
	other[31] ^= 0xff
	if indexDocID(other) == got {
		t.Error("distinct message ids must encode distinctly")
	}

	// And it round-trips, which is what lets the answerer map hex verdicts back
	// to the raw ids uidByMid is keyed by.
	back, err := hex.DecodeString(got)
	if err != nil || string(back) != string(raw) {
		t.Errorf("round trip failed: err=%v back=%x want=%x", err, back, raw)
	}
}
