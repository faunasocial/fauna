package atprotorepo

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/bluesky-social/indigo/atproto/atdata"
)

// JSONRecordToDagCBOR converts an atproto record's JSON — as produced by the
// shared-Rust translator over FFI (AtprotoTranslatePostRecord /
// AtprotoTranslateProfileRecord) — into canonical dag-cbor bytes via indigo's
// atdata encoder, the same spec-pinned encoder that produced the S0 probe's
// records. The result is what the projection loop hands to Funnel.ApplyBatch as
// RepoOp.RecordCBOR.
//
// JSON numbers are decoded with UseNumber and coerced to int64: dag-cbor
// forbids floats, and atproto record integer fields (facet byteStart/byteEnd,
// aspect-ratio width/height, …) would otherwise arrive as float64 and either
// mis-encode or be rejected. A genuinely non-integer number is an error rather
// than a silent float.
func JSONRecordToDagCBOR(recordJSON string) ([]byte, error) {
	dec := json.NewDecoder(strings.NewReader(recordJSON))
	dec.UseNumber()
	var v any
	if err := dec.Decode(&v); err != nil {
		return nil, fmt.Errorf("parse record json: %w", err)
	}
	coerced, err := coerceJSONNumbers(v)
	if err != nil {
		return nil, err
	}
	m, ok := coerced.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("record json is not a JSON object")
	}
	b, err := atdata.MarshalCBOR(m)
	if err != nil {
		return nil, fmt.Errorf("encode record dag-cbor: %w", err)
	}
	return b, nil
}

// DagCBORRecordToJSONValue decodes stored dag-cbor record bytes back into a
// JSON-serializable value — the `value` field of com.atproto.repo.getRecord /
// listRecords. It round-trips through the same indigo atdata codec the forward
// path uses, so a record encoded by JSONRecordToDagCBOR decodes to an
// equivalent shape (CID links + bytes render in atdata's JSON form).
func DagCBORRecordToJSONValue(recordCBOR []byte) (any, error) {
	m, err := atdata.UnmarshalCBOR(recordCBOR)
	if err != nil {
		return nil, fmt.Errorf("decode record dag-cbor: %w", err)
	}
	return m, nil
}

// coerceJSONNumbers walks a UseNumber-decoded JSON tree, replacing every
// json.Number with an int64. It errors on any non-integer number, since no
// atproto record field is a non-integer and dag-cbor cannot represent one.
func coerceJSONNumbers(v any) (any, error) {
	switch t := v.(type) {
	case map[string]any:
		for k, val := range t {
			c, err := coerceJSONNumbers(val)
			if err != nil {
				return nil, err
			}
			t[k] = c
		}
		return t, nil
	case []any:
		for i, val := range t {
			c, err := coerceJSONNumbers(val)
			if err != nil {
				return nil, err
			}
			t[i] = c
		}
		return t, nil
	case json.Number:
		i, err := t.Int64()
		if err != nil {
			return nil, fmt.Errorf("non-integer number %q in record json (dag-cbor forbids floats)", t.String())
		}
		return i, nil
	default:
		return v, nil
	}
}
