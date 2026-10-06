// Tests for the per-rule `Forward` filter action's dispatch arm
// (docs/goal/behavior/mail-forwarding.md § Per-rule "forward to", § Loop
// detection, § Architectural rules). A fired Forward routes into the shared
// dispatchForward pipeline the forward-all stage already uses, AFTER the local
// delivery commits, stamping `rule=<filter-id>` into X-Fauna-Forwarded-By.
// tier_1: the full Data() flow in-process against a mockCaller.
package mta

import (
	"bytes"
	"encoding/hex"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// forwardRule is a stored filter whose single SubjectContains condition fires on
// "fwd" and whose action forwards to dest in `copy` mode. Its action is the
// wire shape with no `redirect` key at all, so every copy-mode test below
// exercises the `#[serde(default)]` reading: an absent key reads as copy
// (`mail-forwarding.md` § Per-rule "forward to").
func forwardRule(t *testing.T, id int64, dest string, cont bool) wsrpc.EmailFilterWire {
	t.Helper()
	return wsrpc.EmailFilterWire{
		ID:          id,
		Name:        "forward rule",
		Combination: "All",
		Rules: []cbor.RawMessage{mustCBOR(t, map[string]any{
			"SubjectContains": map[string]any{"text": "fwd"},
		})},
		Action:          mustCBOR(t, map[string]any{"Forward": map[string]any{"address": dest}}),
		Priority:        int32(id),
		ContinueOnMatch: cont,
	}
}

// redirectRule is forwardRule in `redirect` copy mode: forward, keep no local
// copy (`mail-forwarding.md` § Per-rule "forward to").
func redirectRule(t *testing.T, id int64, dest string, cont bool) wsrpc.EmailFilterWire {
	t.Helper()
	r := forwardRule(t, id, dest, cont)
	r.Action = mustCBOR(t, map[string]any{"Forward": map[string]any{"address": dest, "redirect": true}})
	return r
}

const filterForwardBody = "From: alice@example.org\r\n" +
	"To: bob@test.example\r\n" +
	"Message-ID: <msg-rule@example.org>\r\n" +
	"Subject: please fwd this\r\n" +
	"\r\n" +
	"hello\r\n"

// TestFilterForwardDispatchesWithRuleStamp pins the happy path: a fired
// Forward keeps the local copy (copy mode — the wire carries no redirect yet)
// and enqueues one forward_message attributed to the rule's owner, to the
// rule's destination, with `rule=<filter-id>` in the loop stamp.
func TestFilterForwardDispatchesWithRuleStamp(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		forwardRule(t, 77, "bob-elsewhere@example.net", false),
	}

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("local delivery: got %d ingests, want 1", got)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1; reqs=%+v", got, mc.forwardRequests)
	}
	fr := mc.forwardRequests[0]
	if !bytes.Equal(fr.actorID, actorBob) {
		t.Errorf("forward actor: got %x, want the rule owner %x", fr.actorID, actorBob)
	}
	if fr.destination != "bob-elsewhere@example.net" {
		t.Errorf("destination: got %q", fr.destination)
	}
	if fr.ruleIDOrForwardAll != "77" {
		t.Errorf("rule: got %q, want the filter id 77", fr.ruleIDOrForwardAll)
	}
	if fr.copyMode != "copy" {
		t.Errorf("copy_mode: got %q, want copy", fr.copyMode)
	}
	if !bytes.Contains(fr.rawMessage, []byte("rule=77")) {
		t.Errorf("forwarded stamp missing rule=77")
	}
}

// TestFilterForwardComposesWithForwardAll: two continue-chained forward rules
// plus forward-all produce three forwards, one per destination (§ Per-rule
// "forward to": "A message that hits 3 forward-rules + has forward-all set
// generates 4 forwards").
func TestFilterForwardComposesWithForwardAll(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob-all@example.net"
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		forwardRule(t, 1, "one@example.net", true),
		forwardRule(t, 2, "two@example.net", false),
	}

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	got := map[string]string{}
	for _, fr := range mc.forwardRequests {
		got[fr.destination] = fr.ruleIDOrForwardAll
	}
	want := map[string]string{
		"one@example.net":     "1",
		"two@example.net":     "2",
		"bob-all@example.net": "forward-all",
	}
	if len(got) != len(want) || len(mc.forwardRequests) != len(want) {
		t.Fatalf("forwards: got %+v, want %+v", got, want)
	}
	for dest, rule := range want {
		if got[dest] != rule {
			t.Errorf("forward to %s: rule %q, want %q", dest, got[dest], rule)
		}
	}
}

// TestFilterForwardSkippedForNullSender: a bounce (MAIL FROM:<>) is delivered
// locally but a matching forward rule never fires (§ Architectural rules —
// "Forward attempts don't run on null-sender messages").
func TestFilterForwardSkippedForNullSender(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		forwardRule(t, 5, "bob-elsewhere@example.net", false),
	}

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("local delivery: got %d ingests, want 1", got)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("null-sender must not forward; got %d", got)
	}
}

// TestFilterForwardLoopSuppressedKeepsLocal: an inbound already carrying our
// own X-Fauna-Forwarded-By stamp is delivered locally and the rule's forward is
// suppressed (§ Loop suppression vs. delivery).
func TestFilterForwardLoopSuppressedKeepsLocal(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		forwardRule(t, 9, "bob-elsewhere@example.net", false),
	}
	body := "X-Fauna-Forwarded-By: actor=" + hex.EncodeToString(actorBob) + "; t=1; rule=9\r\n" +
		filterForwardBody

	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("local delivery: got %d ingests, want 1", got)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("self-seen loop must suppress the forward; got %d", got)
	}
}

// TestFilterForwardSuppressedByLaterDiscard: a Discard later in a continue
// chain drops the recipient, so nothing is delivered and the earlier Forward
// never fires (forwards run only after local delivery commits — § Don't do
// these; the same short-circuit AutoReply obeys).
func TestFilterForwardSuppressedByLaterDiscard(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	discard := forwardRule(t, 2, "", false)
	discard.Action = mustCBOR(t, "Discard")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		forwardRule(t, 1, "bob-elsewhere@example.net", true),
		discard,
	}

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Fatalf("discarded recipient: got %d ingests, want 0", got)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("discarded recipient must not forward; got %d", got)
	}
}

// ── redirect copy mode (mail-forwarding.md § Per-rule "forward to") ──────────

// TestFilterForwardRedirectSkipsLocalCopy: a fired `redirect` Forward enqueues
// the forward (copy_mode=redirect, original_msgid = the source Message-ID since
// there is no local ingest to fall back to) and places NOTHING locally — the
// forward's durable enqueue takes the place of the local commit.
func TestFilterForwardRedirectSkipsLocalCopy(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		redirectRule(t, 31, "bob-elsewhere@example.net", false),
	}

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Fatalf("redirect must keep no local copy: got %d ingests, want 0", got)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Fatalf("forward count: got %d, want 1; reqs=%+v", got, mc.forwardRequests)
	}
	fr := mc.forwardRequests[0]
	if fr.copyMode != "redirect" {
		t.Errorf("copy_mode: got %q, want redirect", fr.copyMode)
	}
	if fr.ruleIDOrForwardAll != "31" {
		t.Errorf("rule: got %q, want the filter id 31", fr.ruleIDOrForwardAll)
	}
	if fr.originalMsgID != "msg-rule@example.org" {
		t.Errorf("original_msgid: got %q, want the source Message-ID", fr.originalMsgID)
	}
	if !bytes.Equal(fr.actorID, actorBob) {
		t.Errorf("forward actor: got %x, want the rule owner %x", fr.actorID, actorBob)
	}
}

// TestFilterForwardRedirectFallsBackToLocalWhenLoopSuppressed: a `redirect`
// rule whose forward the self-seen loop floor suppresses still lands the mail
// — locally, as the backstop — instead of losing it (§ Loop suppression vs.
// delivery extends to redirect: "no forward could be enqueued" ⇒ local copy).
func TestFilterForwardRedirectFallsBackToLocalWhenLoopSuppressed(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		redirectRule(t, 32, "bob-elsewhere@example.net", false),
	}
	body := "X-Fauna-Forwarded-By: actor=" + hex.EncodeToString(actorBob) + "; t=1; rule=32\r\n" +
		filterForwardBody

	if err := s.Data(bytes.NewReader([]byte(body))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("self-seen loop must suppress the forward; got %d", got)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("a suppressed redirect must fall back to local delivery: got %d ingests, want 1", got)
	}
}

// TestFilterForwardRedirectFallsBackToLocalWhenNestRefuses: nest refuses a
// `redirect` forward it cannot park without evicting the only copy of other
// accepted mail (mail-forwarding.md § Queue ceiling) — `forward_message`
// errors, nothing was enqueued, so the recipient keeps the mail locally.
func TestFilterForwardRedirectFallsBackToLocalWhenNestRefuses(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	mc.forwardMessageErr = errForwardRefused
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		redirectRule(t, 33, "bob-elsewhere@example.net", false),
	}

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("a refused redirect is delivered locally, never tempfailed: %v", err)
	}
	if got := len(mc.forwardRequests); got != 1 {
		t.Errorf("the redirect is attempted exactly once; got %d", got)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("a refused redirect must fall back to local delivery: got %d ingests, want 1", got)
	}
}

// TestFilterForwardRedirectNullSenderDeliversLocally: a bounce (MAIL FROM:<>)
// matching a `redirect` rule is never forwarded (§ Don't do these) and so is
// delivered locally — the fallback, never a silent drop.
func TestFilterForwardRedirectNullSenderDeliversLocally(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		redirectRule(t, 33, "bob-elsewhere@example.net", false),
	}

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("null-sender must not forward; got %d", got)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("null-sender redirect must deliver locally: got %d ingests, want 1", got)
	}
}

// TestFilterForwardRedirectComposesAsRecipientDecision: redirect is a
// per-recipient placement decision — one fired `redirect` rule in a continue
// chain suppresses the local copy for the whole recipient, and every forward
// then dispatched for that message (the chain's `copy` rule and forward-all
// included) is truthfully copy_mode=redirect, since no local copy exists.
func TestFilterForwardRedirectComposesAsRecipientDecision(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	mc.forwardConfigs[hex.EncodeToString(actorBob)] = "bob-all@example.net"
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		forwardRule(t, 1, "copy@example.net", true),
		redirectRule(t, 2, "redirect@example.net", false),
	}

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Fatalf("a redirect in the chain must suppress the local copy: got %d ingests, want 0", got)
	}
	got := map[string]string{}
	for _, fr := range mc.forwardRequests {
		got[fr.destination] = fr.copyMode
	}
	want := map[string]string{
		"copy@example.net":     "redirect",
		"redirect@example.net": "redirect",
		"bob-all@example.net":  "redirect",
	}
	if len(mc.forwardRequests) != len(want) {
		t.Fatalf("forwards: got %+v, want %+v", got, want)
	}
	for dest, mode := range want {
		if got[dest] != mode {
			t.Errorf("forward to %s: copy_mode %q, want %q", dest, got[dest], mode)
		}
	}
}

// TestFilterForwardRedirectSuppressedByLaterDiscard: Discard stays terminal
// over a redirect Forward exactly as over a copy one — nothing delivered,
// nothing forwarded.
func TestFilterForwardRedirectSuppressedByLaterDiscard(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	actorBob := bytes.Repeat([]byte{0x42}, 32)
	s := forwardingSession(t, mc, actorBob, "alice@example.org")
	discard := forwardRule(t, 2, "", false)
	discard.Action = mustCBOR(t, "Discard")
	mc.filterRules[hex.EncodeToString(actorBob)] = []wsrpc.EmailFilterWire{
		redirectRule(t, 1, "bob-elsewhere@example.net", true),
		discard,
	}

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("Data: %v", err)
	}
	if got := len(mc.ingestRequests); got != 0 {
		t.Fatalf("discarded recipient: got %d ingests, want 0", got)
	}
	if got := len(mc.forwardRequests); got != 0 {
		t.Errorf("discarded recipient must not forward; got %d", got)
	}
}
