//go:build !fauna_e2e_seize

// The production twin of seize.go: the hostile-rotation seam does not exist in
// this build. Convention 15 (`docs/goal/architecture/testing.md`) — the
// automation surface is compiled out of release artifacts, and a runtime gate is
// not enough. The tag is the outer boundary, so a production binary carries
// neither the flags nor the capability behind them: nothing to reach, nothing to
// misconfigure.
//
// The signatures below are the contract seize.go implements. Keep them in sync.
package main

import (
	"context"
	"flag"
	"io"
)

// registerSeizeFlags is a no-op here: the flags exist only in the e2e flavor, so
// `--seize-did` is an unknown flag a production binary refuses to parse.
func registerSeizeFlags(*flag.FlagSet) {}

// maybeRunSeize always declines to handle the run.
func maybeRunSeize(context.Context, string, string, string, io.Writer) (bool, error) {
	return false, nil
}
