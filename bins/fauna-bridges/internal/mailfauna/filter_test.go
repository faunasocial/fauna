package mailfauna

import (
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
)

// mustRaw CBOR-encodes v to a RawMessage, mimicking how nest's
// serde_ipld_dagcbor encodes a value. Struct-variant serde enums encode as a
// single-key map (`{"Variant": {fields}}`); unit variants as a bare string.
func mustRaw(t *testing.T, v any) cbor.RawMessage {
	t.Helper()
	b, err := cbor.Marshal(v)
	if err != nil {
		t.Fatalf("cbor.Marshal(%v): %v", v, err)
	}
	return b
}

// TestStoredFiltersFromWire_DecodesEnums asserts the transport-level
// externally-tagged rule/action enums (the wire shape nest's
// `fetch_recipient_filters` reply carries) decode into the matching
// `fauna_mail::filter` variants the evaluator consumes. This is the Go side of
// the cross-language wire contract; the tier_3 e2e proves the real nest encode
// matches.
func TestStoredFiltersFromWire_DecodesEnums(t *testing.T) {
	wire := []wsrpc.EmailFilterWire{
		{
			ID:   7,
			Name: "newsletters",
			Rules: []cbor.RawMessage{
				mustRaw(t, map[string]any{"SenderDomain": map[string]any{"domain": "lists.example.com"}}),
				mustRaw(t, map[string]any{"HeaderContains": map[string]any{"name": "List-Id", "value": "golang"}}),
			},
			Combination: "all",
			Action:      mustRaw(t, map[string]any{"FileInto": map[string]any{"mailbox": "Newsletters"}}),
			Priority:    10,
			CreatedAt:   1_700_000_000,
		},
		{
			ID:          9,
			Name:        "high spam → discard",
			Rules:       []cbor.RawMessage{mustRaw(t, map[string]any{"SpamScoreAtLeast": map[string]any{"milli": 12000}})},
			Combination: "any",
			Action:      mustRaw(t, "Discard"), // unit variant = bare string
			Priority:    1,
			CreatedAt:   1_700_000_001,
		},
		{
			// The Forward action's additive copy mode: an absent
			// `redirect` key decodes via the serde default (→ copy); the nest
			// always serializes it explicitly.
			ID:          11,
			Name:        "forward, copy mode (no key)",
			Rules:       []cbor.RawMessage{mustRaw(t, map[string]any{"SenderIs": map[string]any{"address": "a@b.example"}})},
			Combination: "all",
			Action:      mustRaw(t, map[string]any{"Forward": map[string]any{"address": "copy@example.net"}}),
			Priority:    2,
			CreatedAt:   1_700_000_002,
		},
		{
			ID:          12,
			Name:        "forward, redirect",
			Rules:       []cbor.RawMessage{mustRaw(t, map[string]any{"SenderIs": map[string]any{"address": "a@b.example"}})},
			Combination: "all",
			Action:      mustRaw(t, map[string]any{"Forward": map[string]any{"address": "redirect@example.net", "redirect": true}}),
			Priority:    3,
			CreatedAt:   1_700_000_003,
		},
	}

	stored, err := StoredFiltersFromWire(wire)
	if err != nil {
		t.Fatalf("StoredFiltersFromWire: %v", err)
	}
	if len(stored) != 4 {
		t.Fatalf("want 4 stored filters, got %d", len(stored))
	}
	if got, want := stored[2].Action, (FilterActionForward{Address: "copy@example.net", Redirect: false}); got != want {
		t.Errorf("forward without redirect key: got %+v, want %+v", got, want)
	}
	if got, want := stored[3].Action, (FilterActionForward{Address: "redirect@example.net", Redirect: true}); got != want {
		t.Errorf("forward with redirect: got %+v, want %+v", got, want)
	}

	// Filter 0: two conditions, All, FileInto.
	f0 := stored[0]
	if f0.Id != 7 || f0.Priority != 10 {
		t.Errorf("f0 id/priority: got id=%d priority=%d", f0.Id, f0.Priority)
	}
	if f0.Combination != FilterCombinationAll {
		t.Errorf("f0 combination: want All, got %v", f0.Combination)
	}
	if len(f0.Conditions) != 2 {
		t.Fatalf("f0 wants 2 conditions, got %d", len(f0.Conditions))
	}
	if c, ok := f0.Conditions[0].(FilterConditionSenderDomain); !ok || c.Domain != "lists.example.com" {
		t.Errorf("f0 cond0: want SenderDomain{lists.example.com}, got %#v", f0.Conditions[0])
	}
	if c, ok := f0.Conditions[1].(FilterConditionHeaderContains); !ok || c.Name != "List-Id" || c.Value != "golang" {
		t.Errorf("f0 cond1: want HeaderContains{List-Id,golang}, got %#v", f0.Conditions[1])
	}
	if a, ok := f0.Action.(FilterActionFileInto); !ok || a.Mailbox != "Newsletters" {
		t.Errorf("f0 action: want FileInto{Newsletters}, got %#v", f0.Action)
	}

	// Filter 1: SpamScoreAtLeast (milli, not "threshold"), Any, Discard.
	f1 := stored[1]
	if f1.Combination != FilterCombinationAny {
		t.Errorf("f1 combination: want Any, got %v", f1.Combination)
	}
	if c, ok := f1.Conditions[0].(FilterConditionSpamScoreAtLeast); !ok || c.Milli != 12000 {
		t.Errorf("f1 cond0: want SpamScoreAtLeast{12000}, got %#v", f1.Conditions[0])
	}
	if _, ok := f1.Action.(FilterActionDiscard); !ok {
		t.Errorf("f1 action: want Discard, got %#v", f1.Action)
	}
}

// TestStoredFiltersFromWire_EvaluatesEndToEnd decodes wire filters and runs the
// shared evaluator, proving the decoded shapes drive a real first-match-wins
// decision (a sender-domain rule files into a custom folder).
func TestStoredFiltersFromWire_EvaluatesEndToEnd(t *testing.T) {
	wire := []wsrpc.EmailFilterWire{{
		ID:          3,
		Name:        "from evil",
		Rules:       []cbor.RawMessage{mustRaw(t, map[string]any{"SenderDomain": map[string]any{"domain": "evil.test"}})},
		Combination: "all",
		Action:      mustRaw(t, map[string]any{"FileInto": map[string]any{"mailbox": "Quarantine"}}),
		Priority:    0,
		CreatedAt:   1,
	}}
	stored, err := StoredFiltersFromWire(wire)
	if err != nil {
		t.Fatalf("StoredFiltersFromWire: %v", err)
	}

	matches := Evaluate(stored, FilterContext{From: "spammer@EVIL.test", Subject: "hi", SpamScoreMilli: 0})
	if len(matches) != 1 {
		t.Fatalf("expected exactly one match on sender domain, got %d", len(matches))
	}
	if matches[0].FilterId != 3 {
		t.Errorf("matched filter id: want 3, got %d", matches[0].FilterId)
	}
	if a, ok := matches[0].Action.(FilterActionFileInto); !ok || a.Mailbox != "Quarantine" {
		t.Errorf("matched action: want FileInto{Quarantine}, got %#v", matches[0].Action)
	}

	// A non-matching sender falls through to no match (→ disposition placement).
	if m := Evaluate(stored, FilterContext{From: "friend@good.test"}); len(m) != 0 {
		t.Errorf("expected no match for non-evil sender, got %#v", m)
	}
}

// TestStoredFiltersFromWire_ContinueMultiMatch proves the `continue_on_match`
// wire bit decodes and drives a multi-action result: a `continue` rule's action
// plus the next matching (terminal) rule's action, in order.
func TestStoredFiltersFromWire_ContinueMultiMatch(t *testing.T) {
	wire := []wsrpc.EmailFilterWire{
		{
			ID:              1,
			Rules:           []cbor.RawMessage{mustRaw(t, map[string]any{"SpamScoreAtLeast": map[string]any{"milli": 0}})},
			Combination:     "all",
			Action:          mustRaw(t, map[string]any{"AddLabel": map[string]any{"label": "flagged"}}),
			Priority:        0,
			ContinueOnMatch: true, // fall through to the next rule
		},
		{
			ID:          2,
			Rules:       []cbor.RawMessage{mustRaw(t, map[string]any{"SpamScoreAtLeast": map[string]any{"milli": 0}})},
			Combination: "all",
			Action:      mustRaw(t, map[string]any{"FileInto": map[string]any{"mailbox": "Bucket"}}),
			Priority:    1,
			// terminal (continue_on_match defaults false)
		},
	}
	stored, err := StoredFiltersFromWire(wire)
	if err != nil {
		t.Fatalf("StoredFiltersFromWire: %v", err)
	}
	if !stored[0].ContinueOnMatch || stored[1].ContinueOnMatch {
		t.Fatalf("continue flag mis-decoded: f0=%v f1=%v", stored[0].ContinueOnMatch, stored[1].ContinueOnMatch)
	}
	matches := Evaluate(stored, FilterContext{SpamScoreMilli: 5})
	if len(matches) != 2 {
		t.Fatalf("continue chain: want 2 matched actions, got %d (%#v)", len(matches), matches)
	}
	if _, ok := matches[0].Action.(FilterActionAddLabel); !ok {
		t.Errorf("first matched action: want AddLabel, got %#v", matches[0].Action)
	}
	if a, ok := matches[1].Action.(FilterActionFileInto); !ok || a.Mailbox != "Bucket" {
		t.Errorf("second matched action: want FileInto{Bucket}, got %#v", matches[1].Action)
	}
}

// TestStoredFiltersFromWire_BodyContains proves a BodyContains wire rule decodes
// and drives a real first-match decision against FilterContext.Body (S4b — body
// matching is in v1 scope: a case-insensitive substring over the decoded body).
func TestStoredFiltersFromWire_BodyContains(t *testing.T) {
	wire := []wsrpc.EmailFilterWire{{
		ID:          11,
		Name:        "invoices",
		Rules:       []cbor.RawMessage{mustRaw(t, map[string]any{"BodyContains": map[string]any{"text": "invoice"}})},
		Combination: "all",
		Action:      mustRaw(t, map[string]any{"FileInto": map[string]any{"mailbox": "Invoices"}}),
		Priority:    0,
		CreatedAt:   1,
	}}
	stored, err := StoredFiltersFromWire(wire)
	if err != nil {
		t.Fatalf("StoredFiltersFromWire: %v", err)
	}
	if c, ok := stored[0].Conditions[0].(FilterConditionBodyContains); !ok || c.Text != "invoice" {
		t.Fatalf("decoded cond: want BodyContains{invoice}, got %#v", stored[0].Conditions[0])
	}

	// Case-insensitive substring over the decoded body matches.
	matches := Evaluate(stored, FilterContext{From: "biller@vendor.test", Body: "Please pay the INVOICE within 30 days."})
	if len(matches) != 1 || matches[0].FilterId != 11 {
		t.Fatalf("expected a body match on filter 11, got %#v", matches)
	}
	if a, ok := matches[0].Action.(FilterActionFileInto); !ok || a.Mailbox != "Invoices" {
		t.Errorf("matched action: want FileInto{Invoices}, got %#v", matches[0].Action)
	}
	// A body without the needle falls through to no match.
	if m := Evaluate(stored, FilterContext{From: "biller@vendor.test", Body: "just a friendly note"}); len(m) != 0 {
		t.Errorf("expected no match without the needle, got %#v", m)
	}
}

// TestFilterBodyFromParts covers the producer-side body assembly + size cap.
func TestFilterBodyFromParts(t *testing.T) {
	// text/plain + text/html concatenated with a newline.
	if got := FilterBodyFromParts("hello", "<p>world</p>"); got != "hello\n<p>world</p>" {
		t.Errorf("concat: got %q", got)
	}
	// Either part alone is returned without a stray separator.
	if got := FilterBodyFromParts("only plain", ""); got != "only plain" {
		t.Errorf("plain-only: got %q", got)
	}
	if got := FilterBodyFromParts("", "<b>only html</b>"); got != "<b>only html</b>" {
		t.Errorf("html-only: got %q", got)
	}
	// Over-cap input is truncated to the cap and stays valid UTF-8 even when
	// the byte cut lands mid-rune (the cap+1'th byte starts a 3-byte rune).
	big := strings.Repeat("a", FilterBodyMaxBytes-1) + "€" // '€' is 3 bytes
	got := FilterBodyFromParts(big, "")
	if len(got) > FilterBodyMaxBytes {
		t.Errorf("cap: got %d bytes, want <= %d", len(got), FilterBodyMaxBytes)
	}
	if !utf8.ValidString(got) {
		t.Errorf("cap: truncated body is not valid UTF-8")
	}
	// The split multi-byte rune at the boundary was dropped, leaving the 'a' run.
	if got != strings.Repeat("a", FilterBodyMaxBytes-1) {
		t.Errorf("cap: want the %d-byte 'a' run with the split rune dropped, got %d bytes", FilterBodyMaxBytes-1, len(got))
	}
}

// TestStoredFiltersFromWire_AllowUnitAction covers the other unit variant.
func TestStoredFiltersFromWire_AllowUnitAction(t *testing.T) {
	wire := []wsrpc.EmailFilterWire{{
		ID:          1,
		Rules:       []cbor.RawMessage{mustRaw(t, map[string]any{"SenderIs": map[string]any{"address": "vip@example.com"}})},
		Combination: "all",
		Action:      mustRaw(t, "Allow"),
	}}
	stored, err := StoredFiltersFromWire(wire)
	if err != nil {
		t.Fatalf("StoredFiltersFromWire: %v", err)
	}
	if _, ok := stored[0].Action.(FilterActionAllow); !ok {
		t.Errorf("want FilterActionAllow, got %#v", stored[0].Action)
	}
}

// TestStoredFiltersFromWire_RejectsMalformed ensures a known rule with
// malformed fields is a hard error (the caller logs + falls through rather than
// mis-filing).
func TestStoredFiltersFromWire_RejectsMalformed(t *testing.T) {
	wire := []wsrpc.EmailFilterWire{{
		ID:          1,
		Rules:       []cbor.RawMessage{mustRaw(t, map[string]any{"SenderIs": map[string]any{"address": 42}})},
		Combination: "all",
		Action:      mustRaw(t, "Allow"),
	}}
	if _, err := StoredFiltersFromWire(wire); err == nil {
		t.Fatal("expected an error for a malformed known rule")
	}
}

// TestStoredFiltersFromWire_UnknownVariantsAreRestrictive pins rule 3's
// restrictive reading (`transport.md` § Schema and forward-compat discipline)
// at the perimeter, filter by filter: an unknown condition never matches (an
// `all` filter holding one never fires; an `any` filter fires on its known
// conditions only), the nest's reading of an undecodable rule list never
// matches, and a filter whose action is unknown does nothing — never Discard —
// while the recipient's other filters still run.
func TestStoredFiltersFromWire_UnknownVariantsAreRestrictive(t *testing.T) {
	sender := mustRaw(t, map[string]any{"SenderIs": map[string]any{"address": "a@b.example"}})
	unknownCond := mustRaw(t, map[string]any{"ListIdIs": map[string]any{"list_id": "x"}})
	undecodableRules := mustRaw(t, []byte{0xff, 0x00}) // the nest carries the raw blob as bytes
	discard := mustRaw(t, "Discard")
	wire := []wsrpc.EmailFilterWire{
		{ID: 1, Rules: []cbor.RawMessage{sender, unknownCond}, Combination: "all", Action: discard, Priority: 1},
		{ID: 2, Rules: []cbor.RawMessage{undecodableRules}, Combination: "all", Action: discard, Priority: 2},
		{ID: 3, Rules: []cbor.RawMessage{sender}, Combination: "all", Action: mustRaw(t, "quarantine:Spam"), Priority: 3},
		{ID: 4, Rules: []cbor.RawMessage{sender}, Combination: "all", Action: mustRaw(t, map[string]any{"Snooze": map[string]any{"hours": 4}}), Priority: 4},
		{ID: 5, Rules: []cbor.RawMessage{unknownCond, sender}, Combination: "any", Action: mustRaw(t, map[string]any{"FileInto": map[string]any{"mailbox": "Lists"}}), Priority: 5},
		{ID: 6, Rules: []cbor.RawMessage{unknownCond}, Combination: "any", Action: discard, Priority: 6},
	}
	stored, err := StoredFiltersFromWire(wire)
	if err != nil {
		t.Fatalf("StoredFiltersFromWire: %v", err)
	}
	matches := Evaluate(stored, FilterContext{From: "a@b.example", Subject: "hi"})
	if len(matches) != 1 || matches[0].FilterId != 5 {
		t.Fatalf("want only filter 5 (any, on its known condition) to fire, got %#v", matches)
	}
	if a, ok := matches[0].Action.(FilterActionFileInto); !ok || a.Mailbox != "Lists" {
		t.Errorf("matched action: want FileInto{Lists}, got %#v", matches[0].Action)
	}
}
