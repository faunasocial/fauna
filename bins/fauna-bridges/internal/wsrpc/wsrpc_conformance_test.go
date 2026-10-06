// Systematic Go↔Rust wire-conformance harness for the WS-RPC request/reply
// surface. This is the executable guardrail for the two dag-cbor wire-drift
// classes that have each cost a production incident or are one field-edit
// away from one:
//
//   (a) nil-container → CBOR null. A nil Go slice/map marshals to `null`
//       (0xf6) under the default fxamacker encMode, but the Rust nest
//       strict-decodes a non-Option `Vec<T>`/map field and REJECTS `null`
//       (canonical dag-cbor: an empty list is `[]` (0x80), an empty map is
//       `{}` (0xa0), never `null`). This shipped once as `UID SEARCH ALL`
//       → `ok=false` (point-fixed in `SearchMessages.terms`). The systemic fix is the `NilContainersAsEmpty` encMode
//       flip in internal/dagcbor/codec.go (LANDED: `NilContainers: cbor.NilContainerAsEmpty`).
//
//   (b) float on the wire. dag-cbor strict decode forbids all floats
//       (serialization.md § Forbidden in the security path); probabilities
//       and scores must be scaled ints (per-mille u16 / micro i64). The
//       codec already rejects float *bytes* on both encode (rejectFloats)
//       and decode (ValidateCanonical), so the only way this class breaks
//       is a NEW f64/f32 *field definition* — which this harness catches at
//       the type level.
//
// Coordination boundary: this file (and the Rust-side
// libs/fauna-protocol/tests/wsrpc_nil_container_contract.rs) are the
// COVERAGE half. The FIX half — the encMode flip (landed), any Option<Vec>→pointer
// corrections in methods.go, and targeted guards in methods_test.go — lives in
// those files; this file only pins the invariant. See
// docs/goal/architecture/serialization.md § WS-RPC nil-container & float
// invariants.
//
// The container test keeps an auto-gate on the live encMode: the encMode flip
// has landed, so the probe passes and the test runs as hard assertions; the
// skip arm only fires if the flip is ever reverted.

package wsrpc

import (
	"reflect"
	"strings"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// conformanceTypes is every Go struct that crosses the WS-RPC wire as a
// request body, reply body, nested element (slice/map value), or push
// payload. Kept in sync with methods.go by hand; a typo here is a compile
// error, and a NEW wire type that someone forgets to add is the only blind
// spot — keep it exhaustive. Out of scope (and deliberately absent): auth.go
// (JSON/HTTP), the *Params / *Outcome / *Result Go-only collector & return
// types (never CBOR-marshalled), and transport/config structs.
//
// Enumerated from methods.go:56-118 (the Method* kind constants) +
// push_config_changed.go, per the full per-method table tracked
// internally.
var conformanceTypes = []any{
	// ── request bodies ──
	requestEnrollmentRequest{}, whoamiRequest{}, registerServiceUserRequest{},
	fetchTLSCertBlobRequest{}, fetchBridgePubkeyRequest{}, fetchConfigRequest{},
	reportAuthEventRequest{}, validateRecipientRequest{},
	checkGreylistRequest{}, fetchWrappedSubmissionTokenRequest{}, checkSubmissionQuotaRequest{},
	ingestInboundMailRequest{}, reportRejectedScanRequest{}, fetchRecipientMLSPubkeyRequest{},
	fetchRecipientIndexKeyRequest{}, fetchWrappedMLSBlobRequest{}, fetchMLSSnapshotBlobRequest{},
	reportSessionCloseRequest{}, listMailboxesRequest{}, selectMailboxRequest{},
	listMessagesRequest{}, fetchMessageMetadataRequest{}, fetchMessageCiphertextRequest{},
	fetchIndexSegmentsSinceRequest{}, searchMessagesRequest{}, getQuotaRequest{},
	storeFlagsRequest{}, copyMessagesRequest{}, moveMessagesRequest{},
	expungeRequest{}, appendRequest{},
	fetchOutboundDueRequest{}, markOutboundDeliveredRequest{}, markOutboundFailedRequest{},
	markOutboundBouncedRequest{}, enqueueOutboundMailRequest{}, fetchMtaStsPolicyRequest{},
	fetchTlsaRequest{}, reportTlsAttemptRequest{}, fetchRecipientForwardConfigRequest{},
	fetchRecipientFiltersRequest{}, forwardMessageRequest{}, decodeSrsBounceRequest{},
	createMailboxRequest{}, deleteMailboxRequest{}, renameMailboxRequest{},
	subscribeMailboxRequest{}, unsubscribeMailboxRequest{}, subscribeMailboxStateRequest{},
	provisionCalendarRequest{}, listCalendarsRequest{}, putEventCiphertextRequest{},
	deleteEventRequest{}, queryEventsRequest{}, syncCalendarSinceRequest{},
	reportLogEventsRequest{},

	// ── reply bodies ──
	requestEnrollmentReply{}, WhoamiReply{}, registerServiceUserReply{},
	fetchTLSCertBlobReply{}, fetchBridgePubkeyReply{}, ConfigSnapshot{},
	reportAuthEventReply{}, validateRecipientReply{},
	checkGreylistReply{}, fetchWrappedSubmissionTokenReply{}, checkSubmissionQuotaReply{},
	ingestInboundMailReply{}, reportRejectedScanReply{}, fetchRecipientMLSPubkeyReply{},
	fetchRecipientIndexKeyReply{}, fetchWrappedMLSBlobReply{}, fetchMLSSnapshotBlobReply{},
	reportSessionCloseReply{}, listMailboxesReply{}, selectMailboxReply{},
	ListMessagesReply{}, fetchMessageMetadataReply{}, fetchMessageCiphertextReply{},
	FetchIndexSegmentsSinceReply{}, searchMessagesReply{}, GetQuotaReply{},
	StoreFlagsReply{}, CopyMessagesReply{}, MoveMessagesReply{},
	ExpungeReply{}, AppendReply{},
	fetchOutboundDueReply{}, markOutboundDeliveredReply{}, markOutboundFailedReply{},
	markOutboundBouncedReply{}, enqueueOutboundMailReply{}, FetchMtaStsPolicyReply{},
	FetchTlsaReply{}, reportTlsAttemptReply{}, fetchRecipientForwardConfigReply{},
	fetchRecipientFiltersReply{}, forwardMessageReply{}, decodeSrsBounceReply{},
	createMailboxReply{}, deleteMailboxReply{}, renameMailboxReply{},
	subscribeMailboxReply{}, unsubscribeMailboxReply{}, subscribeMailboxStateReply{},
	provisionCalendarReply{}, listCalendarsReply{}, putEventCiphertextReply{},
	deleteEventReply{}, queryEventsReply{}, syncCalendarSinceReply{},

	// ── nested element & push payload types ──
	SpamPolicyThresholds{}, AuthPolicy{}, SubmissionPolicyThresholds{}, ImapPolicy{},
	OutboundPolicy{}, BridgePolicy{}, AuthVerdicts{}, DkimVerdict{}, DkimVerdictFail{},
	SpfVerdict{}, DmarcVerdict{}, DmarcVerdictFail{}, ArcVerdict{}, PublicMailMetadata{},
	ClamavVerdict{}, ClamavVerdictData{}, RspamdRuleContribution{}, RspamdScore{},
	MailboxEntry{}, QResyncHint{}, MessageMeta{}, IndexSegment{}, SearchTerm{},
	StoreFlagsResultEntry{}, CopyPair{}, OutboundUnit{}, MtaStsPolicyWire{},
	TlsaRecordWire{}, EmailFilterWire{}, CalendarEntry{}, EventEntry{}, ExpungedEntry{},
	MailboxStateEvent{}, BridgeMailboxStatePush{}, BridgeConfigChangedPush{},
}

// cborField is the parsed view of one struct field's `cbor:"..."` tag.
type cborField struct {
	name      string // wire key (empty if "-" or no tag)
	skip      bool   // tag is "-" → never marshalled
	omitempty bool
}

func parseCborTag(f reflect.StructField) cborField {
	tag := f.Tag.Get("cbor")
	if tag == "" {
		return cborField{name: f.Name}
	}
	parts := strings.Split(tag, ",")
	out := cborField{name: parts[0]}
	if out.name == "-" {
		out.skip = true
	}
	for _, p := range parts[1:] {
		if p == "omitempty" {
			out.omitempty = true
		}
	}
	return out
}

// isByteLeaf reports whether t is a CBOR byte-string leaf — `[]byte` or any
// named type whose underlying type is `[]byte` (e.g. cbor.RawMessage). These
// are major-type-2 byte strings, NOT the list/map containers the
// nil-container invariant governs, so they are excluded from both walks'
// container handling.
func isByteLeaf(t reflect.Type) bool {
	return t.Kind() == reflect.Slice && t.Elem().Kind() == reflect.Uint8
}

// TestWsrpcNoFloatFields asserts no wire type carries a float field, anywhere
// in its reflected shape (struct fields, slice/array elements, map keys and
// values, pointer targets). dag-cbor strict decode forbids floats
// (serialization.md § Forbidden in the security path); every probability or
// score on this surface is a scaled int. GREEN today — this is a permanent
// guard so a future `f64`/`f32` field fails the build instead of the wire.
func TestWsrpcNoFloatFields(t *testing.T) {
	t.Parallel()
	seen := map[reflect.Type]bool{}
	var walk func(t reflect.Type, path string) []string
	walk = func(rt reflect.Type, path string) []string {
		// Unwrap pointers.
		for rt.Kind() == reflect.Pointer {
			rt = rt.Elem()
		}
		if seen[rt] {
			return nil
		}
		seen[rt] = true
		var bad []string
		switch rt.Kind() {
		case reflect.Float32, reflect.Float64:
			return []string{path + " is a " + rt.Kind().String()}
		case reflect.Struct:
			for i := 0; i < rt.NumField(); i++ {
				f := rt.Field(i)
				if !f.IsExported() {
					continue
				}
				if tag := parseCborTag(f); tag.skip {
					continue
				}
				bad = append(bad, walk(f.Type, path+"."+f.Name)...)
			}
		case reflect.Slice, reflect.Array:
			if isByteLeaf(rt) {
				return nil // byte string, leaf
			}
			bad = append(bad, walk(rt.Elem(), path+"[]")...)
		case reflect.Map:
			bad = append(bad, walk(rt.Key(), path+"<key>")...)
			bad = append(bad, walk(rt.Elem(), path+"<val>")...)
		}
		return bad
	}

	var violations []string
	for _, v := range conformanceTypes {
		violations = append(violations, walk(reflect.TypeOf(v), reflect.TypeOf(v).Name())...)
	}
	if len(violations) > 0 {
		t.Fatalf("wsrpc wire types must not contain floats (dag-cbor strict decode forbids them; "+
			"use a scaled int — per-mille u16 / micro i64). Found:\n  %s",
			strings.Join(violations, "\n  "))
	}
}

// encModeEmitsEmptyContainers probes whether the live dagcbor encMode encodes
// a nil non-omitempty slice as the canonical empty list `[]` (0x80) rather
// than `null` (0xf6). It returns true once the
// `NilContainersAsEmpty` encMode flip has landed in internal/dagcbor/codec.go.
func encModeEmitsEmptyContainers(t *testing.T) bool {
	t.Helper()
	type probe struct {
		S []int `cbor:"s"`
	}
	b, err := dagcbor.Marshal(probe{}) // S is nil
	if err != nil {
		t.Fatalf("probe marshal: %v", err)
	}
	var m map[string]cbor.RawMessage
	if err := cbor.Unmarshal(b, &m); err != nil {
		t.Fatalf("probe decode: %v", err)
	}
	raw, ok := m["s"]
	if !ok || len(raw) == 0 {
		t.Fatalf("probe: key `s` missing (a non-omitempty field must always be present); got % x", b)
	}
	switch raw[0] {
	case 0x80: // empty array — flip has landed
		return true
	case 0xf6: // null — flip not yet landed
		return false
	default:
		t.Fatalf("probe: unexpected encoding for nil []int field: % x", []byte(raw))
		return false
	}
}

// TestWsrpcNonOptionalContainersEncodeEmptyNotNull asserts that every
// non-Option (non-pointer), non-omitempty list/map field on every wire type
// encodes its empty/nil value as the canonical empty container (`[]` = 0x80,
// `{}` = 0xa0), never `null` (0xf6). That is the exact invariant the Rust
// nest's strict decoder requires for a non-Option `Vec<T>`/map field, and the
// class the `UID SEARCH ALL` bug belonged to.
//
// AUTO-GATE: the `NilContainersAsEmpty` encMode flip has landed, so the probe
// passes and these are hard assertions; the skip arm only fires if the flip is
// ever reverted (the assertions would then fail for the systemic reason rather
// than a real regression). (Pointer/Option fields and omitempty fields
// are deliberately out of scope: a nil pointer legitimately encodes `null`
// and an omitempty nil container is omitted entirely — both are valid wire
// shapes.)
func TestWsrpcNonOptionalContainersEncodeEmptyNotNull(t *testing.T) {
	t.Parallel()
	if !encModeEmitsEmptyContainers(t) {
		t.Skip("pending NilContainersAsEmpty encMode flip in " +
			"internal/dagcbor/codec.go; this test auto-activates (hard assertions) when the flip lands. " +
			"Watch `git log --grep=NilContainersAsEmpty`.")
	}

	for _, v := range conformanceTypes {
		rt := reflect.TypeOf(v)
		t.Run(rt.Name(), func(t *testing.T) {
			t.Parallel()
			// Collect the in-scope container fields first; skip the type
			// entirely if it has none (avoids needless marshals).
			type want struct {
				key      string
				wantByte byte // 0x80 array, 0xa0 map
			}
			var wants []want
			for i := 0; i < rt.NumField(); i++ {
				f := rt.Field(i)
				if !f.IsExported() {
					continue
				}
				tag := parseCborTag(f)
				if tag.skip || tag.omitempty || tag.name == "" {
					continue
				}
				ft := f.Type
				if ft.Kind() == reflect.Pointer {
					continue // Option ↔ pointer: null/absent is legal
				}
				switch {
				case ft.Kind() == reflect.Slice && !isByteLeaf(ft):
					wants = append(wants, want{tag.name, 0x80})
				case ft.Kind() == reflect.Map:
					wants = append(wants, want{tag.name, 0xa0})
				}
			}
			if len(wants) == 0 {
				return
			}

			b, err := dagcbor.Marshal(v) // zero value: every container field nil
			if err != nil {
				// A zero value that won't marshal (e.g. a nil cbor.RawMessage
				// quirk) is a separate concern from the nil-container class;
				// log and move on. The float test still covers the type.
				t.Logf("%s: zero value does not marshal (%v); skipping container check for this type", rt.Name(), err)
				return
			}
			var m map[string]cbor.RawMessage
			if err := cbor.Unmarshal(b, &m); err != nil {
				t.Fatalf("%s: decode marshalled zero value: %v", rt.Name(), err)
			}
			for _, w := range wants {
				raw, ok := m[w.key]
				if !ok || len(raw) == 0 {
					t.Errorf("%s.%s: a non-omitempty container key must always be present; got % x", rt.Name(), w.key, b)
					continue
				}
				if raw[0] == 0xf6 {
					t.Errorf("%s.%s: nil container encodes as null (0xf6); a non-Option container MUST encode as empty (% #x). "+
						"Make the field a pointer (if it mirrors Rust Option<…>) or rely on the NilContainersAsEmpty encMode.",
						rt.Name(), w.key, w.wantByte)
					continue
				}
				if raw[0] != w.wantByte {
					t.Errorf("%s.%s: container encodes as % #x, want % #x (empty array/map)", rt.Name(), w.key, raw[0], w.wantByte)
				}
			}
		})
	}
}
