package webdav

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// A served set can list a row with no content-key generation: an owner-device
// head recorded before serving started rests under the owner root until the
// served re-seal walk re-seals it, and a head the walk skips stays unstamped
// for good (`writer-signed-change-records.md` ruling (7)(b)(i), rule (3)).
// COPY and MOVE must refuse such a source the way GET does — stamping the copy
// with the current generation names one its bytes were never sealed under, and
// a MOVE would then tombstone the only readable copy.

// listOnlyNest answers the listing and records every method it is sent; any
// other method fails, so a refusal that leaks past the listing is visible.
type listOnlyNest struct {
	mu    sync.Mutex
	files []wsrpc.WebdavFile
	calls []string
}

func (f *listOnlyNest) Call(_ context.Context, method string, _ any, reply any) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, method)
	if method != wsrpc.MethodWebdavListFiles {
		return fmt.Errorf("copy/move of a nil-stamp source sent %s", method)
	}
	return roundTrip(map[string]any{"files": f.files}, reply)
}

func TestCopyAndMoveRefuseANilStampSource(t *testing.T) {
	for _, isMove := range []bool{false, true} {
		nest := &listOnlyNest{files: []wsrpc.WebdavFile{{
			Path:         "notes.txt",
			ManifestHash: strings.Repeat("ab", 32),
			SizeBytes:    11,
			UpdatedAt:    1_700_000_000_000,
		}}}
		sess := davauth.NewSession(nest, nil, []byte{0x01}, "u", "example.test")
		ctx := davauth.ContextWithSession(context.Background(), sess)

		_, err := NewBackend(nil, nest, nil).copyOrMove(ctx, "/webdav/u/docs/notes.txt", "/webdav/u/docs/copy.txt", isMove)

		if err == nil || !strings.Contains(err.Error(), "has no content-key generation") {
			t.Fatalf("move=%v: err = %v, want errNoGeneration, the refusal GET answers for a nil-stamp row", isMove, err)
		}
		if len(nest.calls) != 1 || nest.calls[0] != wsrpc.MethodWebdavListFiles {
			t.Fatalf("move=%v: sent %v, want the listing alone — no record, no tombstone", isMove, nest.calls)
		}
	}
}
