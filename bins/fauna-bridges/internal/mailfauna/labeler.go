package mailfauna

// Community-labeler WASM execution surface — the mailfauna wrappers over the
// shared fauna-ffi `labeler` exports (labeler-registry design §6). The MDA
// re-score drain (Slice 3b) calls these to run a subscribed labeler's `label()`
// over unsealed mail off the nest. All crypto (signature + wasm_hash re-verify,
// B1), the publisher-limit clamp (F4), input mapping, and `Vec<Label>` decoding
// live in shared Rust — Go only shuttles bytes.

import (
	"fmt"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// MailToLabelerInputBare maps a raw RFC-5322 message to the BARE-encoded
// LabelerPostInput the `label()` ABI expects (subject + body, CRLF-normalized).
func MailToLabelerInputBare(rawRFC5322 []byte) ([]byte, error) {
	bare, err := faunaFfi.MailToLabelerInputBare(rawRFC5322)
	if err != nil {
		return nil, fmt.Errorf("MailToLabelerInputBare: %w", err)
	}
	return bare, nil
}

// RunWasmLabelerScore runs a subscribed labeler over one item's BARE-encoded
// input and returns its per-mille [0,1000] tier-3 bus score. The shared FFI
// decodes + re-verifies the signed metadata against the module bytes (B1),
// refuses a module whose signed algorithm_id is not expectedLabelerID — the
// 32-byte id the drain parsed from the `labeler:<hex>` factor it is scoring
// for, so a nest answering inspect(A) with another publisher's module runs
// nothing — clamps the self-signed resource limits to the
// host ceiling (F4), runs the fuel/memory-bounded sandbox, and maps the
// `Vec<Label>` output to the primary per-mille score — so the holder passes
// the raw inspect() bytes straight through.
func RunWasmLabelerScore(metadataBlob, wasmBytes, expectedLabelerID, inputBare []byte) (int64, error) {
	score, err := faunaFfi.RunWasmLabelerScore(metadataBlob, wasmBytes, expectedLabelerID, inputBare)
	if err != nil {
		return 0, fmt.Errorf("RunWasmLabelerScore: %w", err)
	}
	return score, nil
}
