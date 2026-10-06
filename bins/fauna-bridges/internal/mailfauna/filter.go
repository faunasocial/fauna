package mailfauna

// T3.3 delivery-time email filter rules (`smtp-server.md` § Email filter
// rules). The pure first-match-wins evaluator lives in shared Rust
// (`fauna_mail::filter`, uniffi-exported as `faunaMail.Evaluate`) — the
// perimeter-scorer split, like `Tokenize` / `DecideSpamDisposition`. This file
// is the single fauna_mail import surface for it: it re-exports the generated
// types under the mailfauna namespace and decodes the WS-RPC
// `fetch_recipient_filters` reply (transport-level `wsrpc.EmailFilterWire`,
// rules/action as raw externally-tagged enum bytes) into the `StoredFilter`
// shape the evaluator consumes.

import (
	"errors"
	"fmt"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaMail "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_mail"
	"github.com/fxamacker/cbor/v2"
)

// Re-exports of the generated `fauna_mail::filter` types so call sites
// (mta/server.go) switch on filter verdicts without importing fauna_mail
// directly (mirrors the SpamDisposition re-exports above).
type (
	StoredFilter      = faunaMail.StoredFilter
	FilterCondition   = faunaMail.FilterCondition
	FilterCombination = faunaMail.FilterCombination
	FilterContext     = faunaMail.FilterContext
	FilterHeader      = faunaMail.FilterHeader
	FilterMatch       = faunaMail.FilterMatch
	FilterAction      = faunaMail.FilterAction

	// FilterAction variants — server.go type-switches on these to apply the
	// matched verdict. `Allow`/`Discard` are unit variants; the rest carry data.
	FilterActionAllow     = faunaMail.FilterActionAllow
	FilterActionDiscard   = faunaMail.FilterActionDiscard
	FilterActionReject    = faunaMail.FilterActionReject
	FilterActionFileInto  = faunaMail.FilterActionFileInto
	FilterActionForward   = faunaMail.FilterActionForward
	FilterActionAutoReply = faunaMail.FilterActionAutoReply
	FilterActionAddLabel  = faunaMail.FilterActionAddLabel

	// FilterCondition variants — re-exported for symmetry with the actions
	// (e.g. tests asserting a decoded rule's shape without importing fauna_mail).
	FilterConditionSenderIs         = faunaMail.FilterConditionSenderIs
	FilterConditionSenderDomain     = faunaMail.FilterConditionSenderDomain
	FilterConditionSubjectContains  = faunaMail.FilterConditionSubjectContains
	FilterConditionBodyContains     = faunaMail.FilterConditionBodyContains
	FilterConditionHeaderExists     = faunaMail.FilterConditionHeaderExists
	FilterConditionHeaderContains   = faunaMail.FilterConditionHeaderContains
	FilterConditionSpamScoreAtLeast = faunaMail.FilterConditionSpamScoreAtLeast
)

// FilterCombination values, re-exported so call sites compare without importing
// fauna_mail directly.
const (
	FilterCombinationAll = faunaMail.FilterCombinationAll
	FilterCombinationAny = faunaMail.FilterCombinationAny
)

// Evaluate runs the shared filter engine: returns the **ordered** matched
// actions (Sieve `continue` — a non-`continue` match is terminal), or an empty
// slice when none match (caller falls through to spam-disposition placement).
// `filters` need not be pre-sorted — the engine sorts by `priority ASC, id ASC`
// internally. Action composition (placement override, label accumulation,
// `Discard` short-circuit) is the caller's job.
func Evaluate(filters []StoredFilter, ctx FilterContext) []FilterMatch {
	return faunaMail.Evaluate(filters, ctx)
}

// FilterBodyMaxBytes caps the decoded body the MTA puts in `FilterContext.Body`
// for `BodyContains` evaluation, so a huge message can't blow the eval or the
// WS-RPC wire. v1 body matching is a simple case-insensitive substring test
// (`smtp-server.md` § Email filter rules); 256 KiB comfortably covers real text
// bodies while bounding the cost.
const FilterBodyMaxBytes = 256 * 1024

// FilterBodyFromParts builds the `FilterContext.Body` input the `BodyContains`
// condition matches against: the message's decoded `text/plain` and (raw,
// un-stripped) `text/html` parts concatenated, capped to FilterBodyMaxBytes on
// a UTF-8 rune boundary. This is the producer side of the body-matching
// contract the shared evaluator (`fauna_mail::filter`) documents — HTML is
// matched as raw decoded markup (no tag-stripping) in v1. Reuses the body the
// MTA already decoded (`ParsedMessage.BodyText`/`BodyHTML`); never re-MIME-walks.
func FilterBodyFromParts(plain, html string) string {
	body := plain
	if html != "" {
		if body != "" {
			body += "\n"
		}
		body += html
	}
	if len(body) <= FilterBodyMaxBytes {
		return body
	}
	// Truncate to the cap, then drop any partial trailing rune the byte cut
	// may have split, so the wire string stays valid UTF-8 (nest's strict
	// dag-cbor text-string decode requires it).
	return strings.ToValidUTF8(body[:FilterBodyMaxBytes], "")
}

// errUnknownVariant marks a rule or action whose variant this build does not
// know: a newer app's or nest's, carried through the nest, or the nest's
// reading of a rule list it could not decode (`transport.md` § Schema and
// forward-compat discipline, rule 3). It is not a decode failure — an unknown
// condition never matches and an unknown action does nothing — so
// StoredFiltersFromWire leaves out what it cannot run instead of failing every
// filter the recipient has.
var errUnknownVariant = errors.New("unknown variant")

// StoredFiltersFromWire decodes the `fetch_recipient_filters` reply rows
// (`wsrpc.EmailFilterWire`) into the `StoredFilter` shape the evaluator
// consumes. `Rules`/`Action` arrive as raw externally-tagged serde-enum CBOR
// (struct variant = single-key map `{"SenderDomain":{"domain":"x"}}`; unit
// variant = bare string `"Allow"`); this decodes them into the matching
// fauna_mail variants.
//
// A variant this build does not know takes the restrictive reading, filter by
// filter: an unknown condition never matches, so an `all` filter holding one
// is left out and an `any` filter keeps only its known conditions; a filter
// whose action is unknown does nothing, so it is left out. A known variant
// with malformed fields is a hard error — the caller logs it and falls through
// to disposition routing rather than mis-filing.
func StoredFiltersFromWire(wire []wsrpc.EmailFilterWire) ([]StoredFilter, error) {
	out := make([]StoredFilter, 0, len(wire))
	for _, w := range wire {
		combination := filterCombinationFromWire(w.Combination)
		conds := make([]FilterCondition, 0, len(w.Rules))
		neverMatches := false
		for i, r := range w.Rules {
			c, err := decodeFilterCondition(r)
			if errors.Is(err, errUnknownVariant) {
				// An `any` filter whose every condition is unknown keeps an
				// empty list, which the evaluator never matches under `any`.
				if combination == faunaMail.FilterCombinationAll {
					neverMatches = true
				}
				continue
			}
			if err != nil {
				return nil, fmt.Errorf("filter id=%d rule[%d]: %w", w.ID, i, err)
			}
			conds = append(conds, c)
		}
		action, err := decodeFilterAction(w.Action)
		if errors.Is(err, errUnknownVariant) {
			continue
		}
		if err != nil {
			return nil, fmt.Errorf("filter id=%d action: %w", w.ID, err)
		}
		if neverMatches {
			continue
		}
		out = append(out, StoredFilter{
			Id:              w.ID,
			Conditions:      conds,
			Combination:     combination,
			Action:          action,
			Priority:        w.Priority,
			ContinueOnMatch: w.ContinueOnMatch,
		})
	}
	return out, nil
}

// filterCombinationFromWire mirrors the Rust `FilterCombination::from_wire`:
// `"any"` (case-insensitive) → Any; anything else (incl. `"all"` and unknown)
// → All (the legacy default).
func filterCombinationFromWire(s string) FilterCombination {
	if strings.EqualFold(s, "any") {
		return faunaMail.FilterCombinationAny
	}
	return faunaMail.FilterCombinationAll
}

// singleKeyEnum decodes a struct-variant serde enum (a single-key CBOR map)
// into its variant tag + the raw payload bytes for the variant's fields.
func singleKeyEnum(raw cbor.RawMessage) (string, cbor.RawMessage, error) {
	var m map[string]cbor.RawMessage
	if err := cbor.Unmarshal(raw, &m); err != nil {
		return "", nil, fmt.Errorf("not a single-key enum map: %w", err)
	}
	if len(m) != 1 {
		return "", nil, fmt.Errorf("enum map must have exactly one key, got %d", len(m))
	}
	for k, v := range m {
		return k, v, nil
	}
	return "", nil, fmt.Errorf("empty enum map")
}

func decodeFilterCondition(raw cbor.RawMessage) (FilterCondition, error) {
	tag, fields, err := singleKeyEnum(raw)
	if err != nil {
		// Not a tagged condition at all: the shape the nest gives a rule list
		// it could not decode, or a newer writer's.
		return nil, fmt.Errorf("%w: condition: %v", errUnknownVariant, err)
	}
	switch tag {
	case "SenderIs":
		var f struct {
			Address string `cbor:"address"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterConditionSenderIs{Address: f.Address}, nil
	case "SenderDomain":
		var f struct {
			Domain string `cbor:"domain"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterConditionSenderDomain{Domain: f.Domain}, nil
	case "SubjectContains":
		var f struct {
			Text string `cbor:"text"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterConditionSubjectContains{Text: f.Text}, nil
	case "BodyContains":
		var f struct {
			Text string `cbor:"text"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterConditionBodyContains{Text: f.Text}, nil
	case "HeaderExists":
		var f struct {
			Name string `cbor:"name"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterConditionHeaderExists{Name: f.Name}, nil
	case "HeaderContains":
		var f struct {
			Name  string `cbor:"name"`
			Value string `cbor:"value"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterConditionHeaderContains{Name: f.Name, Value: f.Value}, nil
	case "SpamScoreAtLeast":
		var f struct {
			Milli int32 `cbor:"milli"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterConditionSpamScoreAtLeast{Milli: f.Milli}, nil
	default:
		return nil, fmt.Errorf("%w: filter condition %q", errUnknownVariant, tag)
	}
}

func decodeFilterAction(raw cbor.RawMessage) (FilterAction, error) {
	// Unit variants (`Allow`/`Discard`) serialize as a bare CBOR string;
	// struct variants as a single-key map. Try the string form first.
	var unit string
	if err := cbor.Unmarshal(raw, &unit); err == nil {
		switch unit {
		case "Allow":
			return faunaMail.FilterActionAllow{}, nil
		case "Discard":
			return faunaMail.FilterActionDiscard{}, nil
		default:
			// Also the nest's reading of a stored action string it does not
			// recognise, which it carries as that bare string.
			return nil, fmt.Errorf("%w: unit filter action %q", errUnknownVariant, unit)
		}
	}
	tag, fields, err := singleKeyEnum(raw)
	if err != nil {
		return nil, fmt.Errorf("%w: action is neither a unit string nor a struct enum: %v", errUnknownVariant, err)
	}
	switch tag {
	case "Reject":
		var f struct {
			Reason string `cbor:"reason"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterActionReject{Reason: f.Reason}, nil
	case "FileInto":
		var f struct {
			Mailbox string `cbor:"mailbox"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterActionFileInto{Mailbox: f.Mailbox}, nil
	case "Forward":
		// `redirect` is additive on the wire (`#[serde(default)]` Rust-side):
		// a rule served without the key is `copy`, so the zero value is the
		// right default here too.
		var f struct {
			Address  string `cbor:"address"`
			Redirect bool   `cbor:"redirect"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterActionForward{Address: f.Address, Redirect: f.Redirect}, nil
	case "AutoReply":
		var f struct {
			Subject       string `cbor:"subject"`
			Body          string `cbor:"body"`
			IntervalHours uint32 `cbor:"interval_hours"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterActionAutoReply{
			Subject:       f.Subject,
			Body:          f.Body,
			IntervalHours: f.IntervalHours,
		}, nil
	case "AddLabel":
		var f struct {
			Label string `cbor:"label"`
		}
		if err := cbor.Unmarshal(fields, &f); err != nil {
			return nil, err
		}
		return faunaMail.FilterActionAddLabel{Label: f.Label}, nil
	default:
		return nil, fmt.Errorf("%w: filter action %q", errUnknownVariant, tag)
	}
}
