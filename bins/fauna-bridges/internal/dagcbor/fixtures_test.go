//go:build fixtures

// Runs the ipld/codec-fixtures canonical-form corpus through this
// package's Marshal/Unmarshal and asserts byte-for-byte round-trip
// canonicality on every fixture that contains only types the bridge's
// strict DAG-CBOR subset accepts.
//
// Gated by the `fixtures` build tag and the DAGCBOR_FIXTURES_DIR env
// var so the per-package unit tests stay fast. The just recipe
// `dagcbor-fixtures` (justfile) clones the corpus to a scratch dir
// (scripts/fetch-dagcbor-fixtures.sh resolves it portably) and
// invokes `go test -tags=fixtures ./internal/dagcbor/`.
//
// Skip classification (not a static whitelist — corpus regen surfaces
// any new shape automatically):
//
//   - contains-float   — the bridge rejects floats by design; the
//     fixture's wire bytes contain a CBOR float marker (major type 7
//     with 25/26/27 immediate).
//   - contains-tag     — the bridge rejects CBOR tags by design (CIDs
//     in DAG-CBOR are tag 42); the fixture contains a tag marker
//     (major type 6).
//
// Both classes are tracked but counted as SKIP, not FAIL. Any other
// failure mode (decode error on a tag/float-free fixture, or a
// re-marshal that doesn't byte-equal the original) is a real FAIL.

package dagcbor

import (
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
)

func TestCodecFixtures(t *testing.T) {
	dir := os.Getenv("DAGCBOR_FIXTURES_DIR")
	if dir == "" {
		t.Skip("DAGCBOR_FIXTURES_DIR not set; the `dagcbor-fixtures` just recipe clones the corpus and sets this var")
	}
	root := filepath.Join(dir, "fixtures")
	entries, err := os.ReadDir(root)
	if err != nil {
		t.Fatalf("read fixtures dir %s: %v", root, err)
	}
	// Stable iteration so a failure log is reproducible.
	sort.Slice(entries, func(i, j int) bool { return entries[i].Name() < entries[j].Name() })

	var (
		pass, skipFloat, skipTag, fail int
		failures                       []string
	)
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		fixtureDir := filepath.Join(root, e.Name())
		dagCBORPath, err := findDagCBORFile(fixtureDir)
		if err != nil {
			// `fixtures/string-Hello world!` and friends contain spaces
			// that show up as separate dir entries in some filesystems;
			// silently skip directories without a .dag-cbor file.
			continue
		}
		raw, err := os.ReadFile(dagCBORPath)
		if err != nil {
			t.Errorf("%s: read: %v", e.Name(), err)
			fail++
			continue
		}
		switch classifyCBOR(raw) {
		case classFloat:
			skipFloat++
			continue
		case classTag:
			skipTag++
			continue
		}
		// Decode into any (Unmarshal[any]) — the most permissive shape;
		// if the wire bytes are not pure tag/float-free DAG-CBOR the
		// decode itself fails.
		v, err := Unmarshal[any](raw)
		if err != nil {
			failures = append(failures, e.Name()+": Unmarshal: "+err.Error())
			fail++
			continue
		}
		got, err := Marshal(v)
		if err != nil {
			failures = append(failures, e.Name()+": re-Marshal: "+err.Error())
			fail++
			continue
		}
		if !equal(got, raw) {
			failures = append(failures, e.Name()+": not byte-equal after round-trip")
			fail++
			continue
		}
		pass++
	}

	t.Logf("codec-fixtures: pass=%d skip-float=%d skip-tag=%d fail=%d (total dirs=%d)",
		pass, skipFloat, skipTag, fail, len(entries))
	if fail > 0 {
		for _, f := range failures {
			t.Errorf("FAIL %s", f)
		}
	}
	// Sanity: if every fixture skipped, the corpus moved out from under
	// us or the classifier is broken. A non-empty pass count is the
	// minimum signal that the runner exercised the codec.
	if pass == 0 && fail == 0 {
		t.Errorf("no fixtures passed and none failed — corpus path %q produced 0 actionable fixtures; check $DAGCBOR_FIXTURES_DIR", dir)
	}
}

// findDagCBORFile returns the path to the single *.dag-cbor file in
// fixtureDir, or an error if none exists.
func findDagCBORFile(fixtureDir string) (string, error) {
	entries, err := os.ReadDir(fixtureDir)
	if err != nil {
		return "", err
	}
	for _, e := range entries {
		if !e.Type().IsRegular() {
			continue
		}
		if strings.HasSuffix(e.Name(), ".dag-cbor") {
			return filepath.Join(fixtureDir, e.Name()), nil
		}
	}
	return "", os.ErrNotExist
}

type cborClass int

const (
	classNeither cborClass = iota
	classFloat
	classTag
)

// classifyCBOR scans the CBOR bytes for a float (major type 7 with
// argument 25/26/27 — half/single/double precision) or a tag (major
// type 6). Returns the first hit found; classNeither otherwise.
//
// This is a structural classifier, not a full parser — it walks major
// types but doesn't validate semantics. It's correct for the corpus
// (which is itself well-formed CBOR) and avoids depending on a CBOR
// decoder that might itself accept what we want to reject.
func classifyCBOR(b []byte) cborClass {
	for i := 0; i < len(b); {
		ib := b[i]
		major := ib >> 5
		info := ib & 0x1F
		i++
		switch major {
		case 6: // tag
			return classTag
		case 7: // simple/float
			if info == 25 || info == 26 || info == 27 {
				return classFloat
			}
			// info 20/21/22/23 are false/true/null/undefined (1 byte total).
			// info 24 means one more byte (simple value). Other infos are
			// reserved; treat as 1-byte simple.
			if info == 24 {
				i++
			}
		case 0, 1: // unsigned int / negative int
			i += argLen(info)
		case 2, 3: // bytes / text
			argL, length := readArg(b, i, info)
			i += argL + length
		case 4, 5: // array / map
			i += argLen(info)
		}
	}
	return classNeither
}

// argLen returns how many extra bytes the immediate "info" field takes.
func argLen(info byte) int {
	switch info {
	case 24:
		return 1
	case 25:
		return 2
	case 26:
		return 4
	case 27:
		return 8
	}
	return 0
}

// readArg returns (extraBytesAfterInfo, decodedLength) for the major
// types 2/3 (where the argument is the byte-length of the payload).
func readArg(b []byte, off int, info byte) (int, int) {
	switch info {
	case 24:
		if off+1 > len(b) {
			return 0, 0
		}
		return 1, int(b[off])
	case 25:
		if off+2 > len(b) {
			return 0, 0
		}
		return 2, int(b[off])<<8 | int(b[off+1])
	case 26:
		if off+4 > len(b) {
			return 0, 0
		}
		return 4, int(b[off])<<24 | int(b[off+1])<<16 | int(b[off+2])<<8 | int(b[off+3])
	case 27:
		if off+8 > len(b) {
			return 0, 0
		}
		return 8, int(b[off])<<56 | int(b[off+1])<<48 | int(b[off+2])<<40 |
			int(b[off+3])<<32 | int(b[off+4])<<24 | int(b[off+5])<<16 |
			int(b[off+6])<<8 | int(b[off+7])
	}
	return 0, int(info)
}

func equal(a, b []byte) bool {
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
