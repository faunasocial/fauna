package atprotorepo

import (
	"bytes"
	"context"
	"errors"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
)

// sectionCountingWriter records one entry per Write call, so a test can tell a
// STREAMED CAR (one write per section) from a materialised one (a single write
// of the finished buffer).
type sectionCountingWriter struct {
	sizes []int
	// failAt, when > 0, makes the Nth Write fail — the only way to observe that
	// the walk is genuinely driven by the writer rather than handed to it at the
	// end.
	failAt int
	buf    bytes.Buffer
}

var errWriterRefused = errors.New("writer refused")

func (s *sectionCountingWriter) Write(p []byte) (int, error) {
	s.sizes = append(s.sizes, len(p))
	if s.failAt > 0 && len(s.sizes) == s.failAt {
		return 0, errWriterRefused
	}
	return s.buf.Write(p)
}

// TestExportRepoToStreamsOneSectionAtATime pins the mechanism that bounds
// com.atproto.sync.getRepo's cost.
//
// getRepo is the one read route a page ceiling cannot reach: the lexicon gives it
// no limit/cursor, and its answer IS the caller's whole live repo. So the bound
// has to be on how the payload is PRODUCED. It used to be materialised twice over
// before the first byte — every reachable block into an in-memory blockstore, then
// a full copy into a bytes.Buffer — which on an unauthenticated route meant one
// anonymous IP could pin ~2x live-repo bytes of resident memory per in-flight
// call, 60 calls a minute under ClassPublicRead.
//
// Two asserts, and the first is the mutation barrier: restoring a
// materialise-then-return implementation collapses the write count to 1 and turns
// this red. The second (a writer that refuses mid-CAR) is the stronger form of the
// same claim — a buffered implementation could not fail at section 3, because
// there would be no section 3 to fail at.
func TestExportRepoToStreamsOneSectionAtATime(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:plc:stream00000000000000000000000"
	rkeys := seedProofRepo(t, st, f, did, key, 6)

	// (1) One Write per CAR section: header, head commit, every reachable MST
	// node, every live record block.
	sw := &sectionCountingWriter{}
	if err := st.ExportRepoTo(ctx, did, sw); err != nil {
		t.Fatalf("ExportRepoTo: %v", err)
	}
	buffered, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatalf("ExportRepo: %v", err)
	}
	if !bytes.Equal(sw.buf.Bytes(), buffered) {
		t.Error("streamed and buffered exports differ — they must be the same bytes by construction (one CAR framing owner)")
	}
	_, blocks := carCIDs(t, buffered)
	wantSections := len(blocks) + 1 // + the CARv1 header section
	if len(sw.sizes) != wantSections {
		t.Errorf("ExportRepoTo made %d Write calls for a %d-block CAR, want %d (header + one per block) — a whole-repo payload must not be materialised before the first byte",
			len(sw.sizes), len(blocks), wantSections)
	}
	if len(rkeys) != 6 || wantSections < 4 {
		t.Fatalf("test repo too small to distinguish streaming: %d sections", wantSections)
	}

	// (2) The walk is driven by the writer: refusing section 3 stops the export
	// there, with exactly the two earlier sections already out.
	failing := &sectionCountingWriter{failAt: 3}
	err = st.ExportRepoTo(ctx, did, failing)
	if !errors.Is(err, errWriterRefused) {
		t.Errorf("ExportRepoTo with a writer failing at section 3 = %v, want the writer's error surfaced", err)
	}
	if got := failing.buf.Len(); got == 0 || got >= len(buffered) {
		t.Errorf("bytes written before the refusal = %d, want a partial prefix of the %d-byte CAR", got, len(buffered))
	}
	if len(failing.sizes) != 3 {
		t.Errorf("writes attempted before giving up = %d, want exactly 3 (it must stop at the failure, not finish the walk)", len(failing.sizes))
	}
}
