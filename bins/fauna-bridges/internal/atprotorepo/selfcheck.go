package atprotorepo

import (
	"bytes"
	"context"
	"fmt"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/repo"
)

// VerifyRepo reloads did's persisted repo the way a relay does — export the
// CARv1, parse it with indigo's own loader, check the commit structure — and,
// when pub is non-nil, verify the head commit's signature against it.
//
// This is the boot self-check the S0 probe pioneered (its selfCheck, in the
// throwaway tools/atproto-s0-probe removed 2026-10-05): the bytes we are about to serve are validated by the same
// library that will judge them, before any consumer sees them. It catches the
// two failure modes that would otherwise surface as a silent relay rejection
// days later — storage that no longer parses, and a repo whose head was signed
// by a key the identity no longer publishes (a re-minted identity).
func (s *Store) VerifyRepo(ctx context.Context, did string, pub atcrypto.PublicKey) error {
	car, err := s.ExportRepo(ctx, did)
	if err != nil {
		return fmt.Errorf("export repo: %w", err)
	}
	commit, _, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(car))
	if err != nil {
		return fmt.Errorf("reload repo from CAR: %w", err)
	}
	if err := commit.VerifyStructure(); err != nil {
		return fmt.Errorf("verify commit structure: %w", err)
	}
	if pub != nil {
		if err := commit.VerifySignature(pub); err != nil {
			return fmt.Errorf("verify commit signature: %w", err)
		}
	}
	return nil
}
